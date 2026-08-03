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
                let (role, content, token_count) = match e {
                    crate::memory::working::WorkingEntry::Hot { role, content, token_count, .. } => {
                        (format!("{:?}", role), content.clone(), *token_count)
                    }
                    crate::memory::working::WorkingEntry::Warm { summary, token_count, .. } => {
                        ("warm".into(), summary.clone(), *token_count)
                    }
                    crate::memory::working::WorkingEntry::Cold { placeholder, token_count, .. } => {
                        ("cold".into(), placeholder.clone(), *token_count)
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
                    source_range: (idx, idx + 1),
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
    /// 2. Auto Compact：全量压缩（最后手段）
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
        }
    }
}
