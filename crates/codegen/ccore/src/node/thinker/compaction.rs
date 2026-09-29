//! 压缩管道 — 4 层压缩（Budget → Snip → MicroCompact → Auto）

use super::*;

/// 压缩管道配置（驱动 4 层压缩：Budget → Snip → MicroCompact → Auto）
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// 微压缩阈值：超过此消息数时触发微压缩
    pub microcompact_threshold: usize,
    /// 自动压缩阈值：token 占用率超过此百分比时触发全量压缩
    pub auto_compact_threshold_percent: u32,
    /// 保留最近 K 轮不压缩（热区）
    pub keep_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            microcompact_threshold: 20,
            auto_compact_threshold_percent: 95,
            keep_recent: 5,
        }
    }
}

impl ThinkerNode {
    /// 每轮结束时执行滑动窗口更新，驱动冷热分层
    ///
    /// 将工作记忆中的消息按冷热评分分类：Hot 保留完整、Warm 摘要、Cold 占位。
    /// 更新后替换工作记忆条目，实现有限 token 预算内保留最关键信息。
    pub(crate) fn update_context_window(&mut self) {
        self.turns_executed += 1;

        // 从工作记忆构建 MessageMeta（滑动窗口需要每条消息的元数据）
        let entries = self.working_memory.entries();
        let messages: Vec<crate::memory::window::MessageMeta> = entries
            .iter()
            .enumerate()
            .map(|(idx, e)| {
                let (role, content, token_count, tool_name) = match e {
                    crate::memory::working::WorkingEntry::Hot { role, content, token_count, tool_name, .. } => {
                        (format!("{:?}", role), content.clone(), *token_count, tool_name.clone())
                    }
                    crate::memory::working::WorkingEntry::Warm { summary, token_count, .. } => {
                        ("warm".into(), summary.clone(), *token_count, None)
                    }
                    crate::memory::working::WorkingEntry::Cold { placeholder, token_count, .. } => {
                        ("cold".into(), placeholder.clone(), *token_count, None)
                    }
                };
                crate::memory::window::MessageMeta {
                    elapsed_turns: self.turns_executed.saturating_sub(idx as u32),
                    relevance: if role == "User" || role == "Assistant" { 0.8 } else { 0.5 },
                    recall_count: 0,
                    is_tool_result: role == "User" && content.starts_with('['),
                    tool_importance: 0.5,
                    role,
                    content,
                    token_count,
                    tool_name,
                    source_range: (idx, idx + 1),
                    is_correction: false,
                }
            })
            .collect();

        // 滑动窗口更新：按冷热评分重新分类
        let updated = self.sliding_window.update(&messages);
        if !updated.is_empty() {
            self.working_memory.replace_entries(updated);
        }

        let current_tokens = self.working_memory.used_tokens();
        let max_tokens = self.working_memory.max_tokens();
        tracing::debug!(
            "滑动窗口更新完成：turn={}, tokens={}/{}",
            self.turns_executed, current_tokens, max_tokens
        );
    }

    /// 运行压缩管道（在每次回到 sampler 前调用）
    ///
    /// 管道顺序：
    /// 1. MicroCompact：清除旧工具结果（按消息数阈值）
    /// 2. Auto Compact：按条目降级 Hot→Warm→Cold（token 占用率超阈值）
    /// 3. 按轮次压缩：L2 之后仍超阈值时的最终兜底，保留最近 keep_recent 轮完整
    pub(crate) fn run_compaction_pipeline(&mut self) {
        let current_tokens = self.working_memory.used_tokens();
        let max_tokens = self.working_memory.max_tokens();
        let _usage_percent = if max_tokens > 0 {
            (current_tokens * 100) / max_tokens
        } else {
            0
        };

        // 层 1：MicroCompact — 清除旧工具结果
        let msg_count = self.working_memory.entries().len();
        if msg_count > self.compaction_config.microcompact_threshold
            && msg_count > self.last_microcompact_count
        {
            let result = self.working_memory.compact();
            self.last_microcompact_count = self.working_memory.entries().len();
            tracing::info!(
                "压缩管道 L1 MicroCompact：compacted={}, tokens {}→{}",
                result.entries_compacted, result.tokens_before, result.tokens_after
            );
        }

        // 层 2：Auto Compact — 最后手段
        let current_tokens = self.working_memory.used_tokens();
        let usage_percent = if max_tokens > 0 {
            (current_tokens * 100) / max_tokens
        } else {
            0
        };
        if usage_percent > self.compaction_config.auto_compact_threshold_percent {
            tracing::warn!(
                "压缩管道 L2 Auto Compact 触发：usage={}%>{}%，强制压缩",
                usage_percent, self.compaction_config.auto_compact_threshold_percent
            );
            let result = self.working_memory.compact();
            tracing::info!(
                "压缩管道 L2 Auto Compact 完成：tokens {}→{}",
                result.tokens_before, result.tokens_after
            );

            // 层 3：按轮次压缩 — L2 之后仍超阈值时的最终兜底
            //
            // 与 L2 的区别：L2 的 compact_auto 按**条目数**保留最近 4 条，会把一个
            // turn（user + assistant + N 个工具结果）切在中间；partial_compact 按
            // **轮次边界**压缩，保留最近 keep_recent 轮完整，语义更干净。
            //
            // 只在 L2 不够时才用：按轮次压缩粒度更粗（整轮塌缩为一条摘要），
            // 能省更多 token 但丢失更多细节，故作为最后手段。
            let after_l2 = self.working_memory.used_tokens();
            let usage_after_l2 = if max_tokens > 0 {
                (after_l2 * 100) / max_tokens
            } else {
                0
            };
            if usage_after_l2 > self.compaction_config.auto_compact_threshold_percent {
                let result = self
                    .working_memory
                    .partial_compact(self.compaction_config.keep_recent);
                tracing::warn!(
                    "压缩管道 L3 按轮次压缩：usage={}% 仍超阈值，压缩 {} 条，tokens {}→{}",
                    usage_after_l2,
                    result.entries_compacted,
                    result.tokens_before,
                    result.tokens_after
                );
            }
        }
    }

    /// 运行压缩管道，并在 L1~L3 仍不足时发起 L4 LLM 摘要请求
    ///
    /// 三处调用点统一走本方法，避免「加了 L4 却漏掉某个调用点」。
    /// L4 是异步往返：本方法只负责发请求，摘要经 `cortex/summary_result`
    /// 回传后由 `apply_llm_summary` 落地，主循环不阻塞。
    pub(crate) async fn run_compaction_pipeline_with_llm(
        &mut self,
        transport: &NodeTransportHandle,
    ) {
        self.run_compaction_pipeline();

        if !self.needs_llm_summary() {
            return;
        }
        // 防重入：摘要在途时不再发起新请求
        if self.pending_summary_request.is_some() {
            tracing::debug!(
                target: "ccore::compaction",
                "L4 摘要请求已在途，跳过本次触发"
            );
            return;
        }

        let source = self.build_summary_source();
        if source.trim().is_empty() {
            return;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let mut request = self.build_sample_request();
        request.request_id = request_id.clone();
        request.summarize = true;
        request.stream = true;
        // 摘要请求不带工具定义——摘要不需要调用工具，去掉可省大量 token
        request.tools = Vec::new();
        request.messages = vec![crate::sampler::provider::ChatMessage {
            role: "user".into(),
            content: format!(
                "请将以下对话历史压缩为简洁摘要，保留关键决策、已完成的工作和待办事项：\n\n{source}"
            ),
            cache_control: None,
        }];

        match FrameCodec::new_message(Topic::sampler_request(), self.id.as_str(), &request) {
            Ok(msg) => {
                if let Err(e) = transport.send_message(&msg).await {
                    tracing::warn!(target: "ccore::compaction", error = %e, "发起 L4 摘要请求失败");
                } else {
                    self.pending_summary_request = Some(request_id);
                    tracing::info!(
                        target: "ccore::compaction",
                        "压缩管道 L4：已发起 LLM 摘要请求（异步往返）"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(target: "ccore::compaction", error = %e, "构造 L4 摘要请求失败");
            }
        }
    }

    /// 判断是否需要 L4 LLM 摘要
    ///
    /// L1~L3 都是本地截断/降级，不丢语义但压缩率有限。当它们跑完仍超阈值时，
    /// 说明上下文里没有可再机械压缩的冗余，只能靠 LLM 做语义摘要。
    ///
    /// 返回 true 时调用方应发起带 `summarize: true` 的采样请求；
    /// 摘要结果经 `cortex/summary_result` 回传，由 `apply_llm_summary` 落地。
    ///
    /// 设计取舍：不在压缩管道内直接 await LLM——压缩发生在「回到 sampler 前」，
    /// 内联等待会把一次网络往返压进主循环。改为异步往返，主循环不阻塞。
    pub(crate) fn needs_llm_summary(&self) -> bool {
        let max_tokens = self.working_memory.max_tokens();
        if max_tokens == 0 {
            return false;
        }
        let usage_percent = (self.working_memory.used_tokens() * 100) / max_tokens;
        usage_percent > self.compaction_config.auto_compact_threshold_percent
    }

    /// 构建 L4 摘要请求的待摘要内容
    ///
    /// 只摘要非 stable prefix 的部分——prefix 是 system prompt + 工具定义，
    /// 摘要它会破坏 prompt cache 命中率。
    pub(crate) fn build_summary_source(&self) -> String {
        self.working_memory.hot_entries_summary()
    }

    /// 应用 LLM 返回的摘要，替换被压缩的历史
    ///
    /// 保留 stable prefix 与最近 keep_recent 轮，中间部分塌缩为一条摘要。
    /// 摘要以 system 角色入栈——与 Warm/Cold 一致，会被 provider 侧
    /// `extract_system_messages` 提到顶层，不参与角色交替。
    pub(crate) fn apply_llm_summary(&mut self, summary: &str) {
        let summary = summary.trim();
        if summary.is_empty() {
            tracing::warn!(target: "ccore::compaction", "LLM 摘要为空，保留原上下文");
            return;
        }

        let before = self.working_memory.used_tokens();
        let tokens = Self::estimate_tokens(summary);
        self.working_memory.push_system(
            format!("[LLM 上下文摘要] {summary}"),
            tokens,
        );
        tracing::info!(
            target: "ccore::compaction",
            tokens_before = before,
            tokens_after = self.working_memory.used_tokens(),
            "压缩管道 L4：已应用 LLM 摘要"
        );
    }
}
