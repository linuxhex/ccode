//! L0 工作记忆 - 当前 LLM context window 内的内容
//!
//! 管理当前上下文窗口中的消息，按冷热评分动态更新
//!
//! ## 消息角色（借鉴 Claude Code message 类型）
//!
//! - `system`: 系统提示词、工具定义、上下文注入
//! - `user`: 用户输入、感官信号、工具返回结果
//! - `assistant`: LLM 响应、工具调用请求
//!
//! 角色必须按 system → user → assistant → user → ... 交替，
//! 不能连续相同角色（LLM API 限制）。

use serde::{Deserialize, Serialize};

use super::embedding::{EmbeddingIndex, EmbeddingVector};
use super::mmr::mmr_select;

/// LLM 摘要客户端 trait
///
/// 抽象 LLM 调用能力，使 WorkingMemory 不直接依赖消息总线。
/// 实现者可以通过消息总线（SamplerNode）或直接 API 调用来完成摘要。
#[async_trait::async_trait]
pub trait LlmSummarizer: Send + Sync {
    /// 调用 LLM 生成摘要
    ///
    /// # Arguments
    /// * `prompt` - 摘要提示词
    /// * `content` - 需要摘要的内容
    ///
    /// # Returns
    /// 摘要后的文本
    async fn summarize(&self, prompt: &str, content: &str) -> Result<String, anyhow::Error>;
}

/// 基于截断的后备摘要器（无需 LLM）
pub struct TruncatingSummarizer {
    /// 截断保留的字符数
    max_chars: usize,
}

impl TruncatingSummarizer {
    pub fn new(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

impl Default for TruncatingSummarizer {
    fn default() -> Self {
        Self { max_chars: 200 }
    }
}

#[async_trait::async_trait]
impl LlmSummarizer for TruncatingSummarizer {
    async fn summarize(&self, _prompt: &str, content: &str) -> Result<String, anyhow::Error> {
        if content.len() <= self.max_chars {
            return Ok(content.to_string());
        }
        Ok(format!(
            "[摘要] {}...",
            content.chars().take(self.max_chars).collect::<String>()
        ))
    }
}

/// 消息角色（标准 LLM API 格式）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::System => write!(f, "system"),
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
        }
    }
}

impl From<MessageRole> for String {
    fn from(role: MessageRole) -> String {
        role.to_string()
    }
}

impl std::convert::TryFrom<&str> for MessageRole {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s.to_lowercase().as_str() {
            "system" => Ok(Self::System),
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            _ => Err(format!("Invalid message role: {}", s)),
        }
    }
}

/// 上下文压缩策略（借鉴 Claude Code CompactionPolicy）
#[derive(Debug, Clone)]
pub struct CompactionPolicy {
    /// 触发自动压缩的上下文使用百分比
    pub auto_compact_threshold_percent: u32,
    /// 压缩时使用的模型（None = 使用当前模型）
    pub compact_model: Option<String>,
    /// 是否在压缩前运行记忆刷写
    pub memory_flush_enabled: bool,
    /// 压缩挂钟时间预算（秒）
    pub wall_clock_budget_secs: u64,
    /// 部分压缩时保留最近N轮对话（Claude Code: partial compaction）
    pub keep_recent_turns: usize,
    /// 压缩失败后是否回退到更小的预算重试
    pub fallback_on_failure: bool,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            auto_compact_threshold_percent: 85,
            compact_model: None,
            memory_flush_enabled: false,
            wall_clock_budget_secs: 300,
            keep_recent_turns: 4,
            fallback_on_failure: true,
        }
    }
}

impl CompactionPolicy {
    /// 自动压缩触发条件（借鉴 Claude Code autoCompact.ts）
    ///
    /// 当token使用量超过80%上下文窗口时自动触发
    pub fn should_auto_compact(&self, token_usage_ratio: f64) -> bool {
        let threshold = self.auto_compact_threshold_percent as f64 / 100.0;
        token_usage_ratio >= threshold
    }
}

/// 压缩结果
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// 压缩前 token 数
    pub tokens_before: u32,
    /// 压缩后 token 数
    pub tokens_after: u32,
    /// 被压缩的条目数
    pub entries_compacted: usize,
    /// 压缩的条目索引范围 (start, end)
    pub compacted_range: (usize, usize),
}

/// 工作记忆中的条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkingEntry {
    /// 热消息：完整原文
    Hot {
        role: MessageRole,
        content: String,
        token_count: u32,
    },
    /// 温消息：压缩摘要
    Warm {
        summary: String,
        token_count: u32,
        source_range: (usize, usize),
    },
    /// 冷消息：占位符，可从 L1 召回
    Cold {
        placeholder: String,
        source_range: (usize, usize),
        token_count: u32,
    },
}

impl WorkingEntry {
    /// 获取此条目占用的 token 数
    pub fn token_count(&self) -> u32 {
        match self {
            Self::Hot { token_count, .. } => *token_count,
            Self::Warm { token_count, .. } => *token_count,
            Self::Cold { token_count, .. } => *token_count,
        }
    }

    /// 是否为冷消息
    pub fn is_cold(&self) -> bool {
        matches!(self, Self::Cold { .. })
    }
}

/// L0 工作记忆
pub struct WorkingMemory {
    /// 当前条目列表
    entries: Vec<WorkingEntry>,
    /// 最大 token 预算
    max_tokens: u32,
    /// 已使用 token 数（增量计数器，O(1) 读取）
    /// 每次 push_hot / compact / replace_entries 时同步更新
    used_tokens_cached: u32,
    /// 压缩策略
    compaction_policy: CompactionPolicy,
    /// Embedding 索引（用于相似度检索）
    embedding_index: EmbeddingIndex,
    /// LLM 摘要器（None 时回退到截断）
    summarizer: Option<std::sync::Arc<dyn LlmSummarizer>>,
}

impl WorkingMemory {
    pub fn new(max_tokens: u32) -> Self {
        Self {
            entries: Vec::new(),
            max_tokens,
            used_tokens_cached: 0,
            compaction_policy: CompactionPolicy::default(),
            embedding_index: EmbeddingIndex::new(),
            summarizer: None,
        }
    }

    /// 设置 LLM 摘要器
    pub fn set_summarizer(&mut self, summarizer: std::sync::Arc<dyn LlmSummarizer>) {
        self.summarizer = Some(summarizer);
    }

    /// 使用 LLM 摘要器或截断回退
    pub async fn summarize_content(&self, content: &str) -> Result<String, anyhow::Error> {
        let prompt = "请将以下对话历史压缩为简洁摘要，保留关键信息：\n";
        if let Some(ref summarizer) = self.summarizer {
            match summarizer.summarize(prompt, content).await {
                Ok(summary) => return Ok(summary),
                Err(e) => {
                    tracing::warn!("LLM 摘要失败，回退到截断：{}", e);
                }
            }
        }
        // 回退到截断
        let fallback = TruncatingSummarizer::default();
        fallback.summarize(prompt, content).await
    }

    /// 当前已用 token 数（O(1)，增量计数器）
    pub fn used_tokens(&self) -> u32 {
        self.used_tokens_cached
    }

    /// 重新计算 token 总数（仅在 replace_entries 等批量操作后调用）
    fn recalc_used_tokens(&mut self) {
        self.used_tokens_cached = self.entries.iter().map(|e| e.token_count()).sum();
    }

    /// 剩余可用 token 数
    pub fn available_tokens(&self) -> u32 {
        self.max_tokens.saturating_sub(self.used_tokens_cached)
    }

    /// 获取最大 token 预算
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// 添加热消息（显式角色）
    pub fn push_hot(&mut self, role: MessageRole, content: String, token_count: u32) {
        self.used_tokens_cached += token_count;
        self.entries.push(WorkingEntry::Hot {
            role,
            content,
            token_count,
        });
    }

    /// 添加系统消息（便捷方法）
    pub fn push_system(&mut self, content: impl Into<String>, token_count: u32) {
        self.push_hot(MessageRole::System, content.into(), token_count);
    }

    /// 添加用户消息（便捷方法）
    pub fn push_user(&mut self, content: impl Into<String>, token_count: u32) {
        self.push_hot(MessageRole::User, content.into(), token_count);
    }

    /// 添加助手消息（便捷方法）
    pub fn push_assistant(&mut self, content: impl Into<String>, token_count: u32) {
        self.push_hot(MessageRole::Assistant, content.into(), token_count);
    }

    /// 获取所有条目
    pub fn entries(&self) -> &[WorkingEntry] {
        &self.entries
    }

    /// 获取可发送给 LLM 的消息列表（OpenAI 格式）
    ///
    /// 输出格式：`Vec<(String, String)>` where `(role, content)`
    /// - role: "system" | "user" | "assistant"
    /// - content: 消息内容
    ///
    /// 借鉴 Claude Code 的消息格式转换逻辑：
    /// - Hot: 直接输出原角色和内容
    /// - Warm: 作为 system 消息，内容为 `[上下文摘要] {summary}`
    /// - Cold: 作为 system 消息，内容为占位符
    pub fn to_chat_messages(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .map(|entry| match entry {
                WorkingEntry::Hot { role, content, .. } => {
                    (role.to_string(), content.clone())
                }
                WorkingEntry::Warm { summary, .. } => {
                    ("system".into(), format!("[上下文摘要] {}", summary))
                }
                WorkingEntry::Cold { placeholder, .. } => {
                    ("system".into(), placeholder.clone())
                }
            })
            .collect()
    }

    /// 直接构建 ChatMessage 列表，避免中间 Vec<(String, String)> 转换
    ///
    /// 性能优化：跳过 `to_chat_messages()` 的中间分配，
    /// 直接输出最终格式，减少一次 Vec 分配和所有元组的分配。
    /// 对于大上下文窗口（数百条消息），可节省显著的开销。
    pub fn to_chat_messages_direct<M>(&self, make_msg: impl Fn(String, String) -> M) -> Vec<M> {
        self.entries
            .iter()
            .map(|entry| match entry {
                WorkingEntry::Hot { role, content, .. } => {
                    make_msg(role.to_string(), content.clone())
                }
                WorkingEntry::Warm { summary, .. } => {
                    make_msg("system".into(), format!("[上下文摘要] {}", summary))
                }
                WorkingEntry::Cold { placeholder, .. } => {
                    make_msg("system".into(), placeholder.clone())
                }
            })
            .collect()
    }

    /// 用滑动窗口结果替换所有条目
    pub fn replace_entries(&mut self, entries: Vec<WorkingEntry>) {
        self.entries = entries;
        self.recalc_used_tokens();
    }

    /// 设置压缩策略
    pub fn set_compaction_policy(&mut self, policy: CompactionPolicy) {
        self.compaction_policy = policy;
    }

    /// 判断是否需要压缩
    pub fn should_compact(&self) -> bool {
        if self.max_tokens == 0 {
            return false;
        }
        let usage_percent = (self.used_tokens_cached as u64 * 100) / self.max_tokens as u64;
        usage_percent >= self.compaction_policy.auto_compact_threshold_percent as u64
    }

    /// 使用当前策略执行压缩
    pub fn compact(&mut self) -> CompactionResult {
        self.compact_with_policy(&self.compaction_policy.clone())
    }

    /// 异步压缩（使用 LLM 智能摘要）
    ///
    /// 使用默认策略执行压缩，尝试使用 LLM 生成智能摘要。
    /// 如果没有提供 summarizer 或 LLM 调用失败，回退到截断方式。
    ///
    /// # Arguments
    /// * `summarizer` - 可选的 LLM 摘要回调函数
    ///
    /// # Returns
    /// 压缩结果，包含压缩前后的 token 数和被压缩的条目数
    pub async fn compact_async<F, Fut>(&mut self, summarizer: Option<F>) -> CompactionResult
    where
        F: Fn(&str) -> Fut,
        Fut: std::future::Future<Output = Result<String, anyhow::Error>>,
    {
        self.compact_with_policy_async(&self.compaction_policy.clone(), summarizer)
            .await
    }

    /// 使用指定策略执行压缩（同步版本，使用截断）
    ///
    /// 策略：
    /// 1. 找到最旧的连续 Hot 条目块（排除最近 4 条保持 Hot）
    /// 2. 将这些 Hot→Warm（截断内容至约 50% 并加 `[已压缩]` 前缀）
    /// 3. 将已有的 Warm→Cold（占位符替换）
    pub fn compact_with_policy(&mut self, _policy: &CompactionPolicy) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;
        let total = self.entries.len();

        // 保持最近的条目为 Hot 的安全边界
        let keep_hot_recent = 4;

        if total <= keep_hot_recent {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        // 第一步：将已有的 Warm 条目降级为 Cold
        for entry in &mut self.entries {
            if let WorkingEntry::Warm {
                summary,
                token_count,
                source_range,
            } = entry
            {
                let placeholder = format!("[冷缓存] {}", &summary[..summary.len().min(40)]);
                let cold_tokens = (*token_count / 4).max(1);
                *entry = WorkingEntry::Cold {
                    placeholder,
                    source_range: *source_range,
                    token_count: cold_tokens,
                };
            }
        }

        // 第二步：将较旧的 Hot 条目降级为 Warm（保留最近 keep_hot_recent 条为 Hot）
        let hot_cutoff = total.saturating_sub(keep_hot_recent);
        let mut compacted_start = total;
        let mut compacted_end = 0;
        let mut entries_compacted = 0usize;

        for i in 0..hot_cutoff {
            // 使用 take 避免 clone：直接替换 entry，取走 content 所有权
            let entry = std::mem::replace(&mut self.entries[i], WorkingEntry::Cold {
                placeholder: String::new(),
                source_range: (i, i),
                token_count: 0,
            });

            if let WorkingEntry::Hot {
                content,
                token_count,
                ..
            } = entry
            {
                // 截断内容至约 50% 并加 [已压缩] 前缀
                // 使用 chars() 处理 UTF-8 边界，避免截断到多字节字符中间
                let truncate_len = (content.len() / 2).max(1);
                let summary = format!("[已压缩] {}...", &content[..content.floor_char_boundary(truncate_len)]);
                let warm_tokens = (token_count / 2).max(1);

                self.entries[i] = WorkingEntry::Warm {
                    summary,
                    token_count: warm_tokens,
                    source_range: (i, i),
                };

                compacted_start = compacted_start.min(i);
                compacted_end = compacted_end.max(i);
                entries_compacted += 1;
            } else {
                // 非 Hot 条目放回
                self.entries[i] = entry;
            }
        }

        self.recalc_used_tokens();

        CompactionResult {
            tokens_before,
            tokens_after: self.used_tokens_cached,
            entries_compacted,
            compacted_range: (compacted_start, compacted_end),
        }
    }

    /// 使用指定策略执行压缩（异步版本，支持 LLM 智能摘要）
    ///
    /// # Arguments
    /// * `policy` - 压缩策略
    /// * `summarizer` - 可选的 LLM 摘要回调函数，如果为 None 则回退到截断
    ///
    /// # Strategy
    /// 1. 找到最旧的连续 Hot 条目块（排除最近 4 条保持 Hot）
    /// 2. 如果提供了 summarizer：
    ///    - 使用 LLM 生成智能摘要
    ///    - 失败时回退到截断
    /// 3. 如果没有提供 summarizer：
    ///    - 使用截断方式（fallback）
    /// 4. 将已有的 Warm→Cold（占位符替换）
    pub async fn compact_with_policy_async<F, Fut>(
        &mut self,
        _policy: &CompactionPolicy,
        summarizer: Option<F>,
    ) -> CompactionResult
    where
        F: Fn(&str) -> Fut,
        Fut: std::future::Future<Output = Result<String, anyhow::Error>>,
    {
        let tokens_before = self.used_tokens_cached;
        let total = self.entries.len();

        // 保持最近的条目为 Hot 的安全边界
        let keep_hot_recent = 4;

        if total <= keep_hot_recent {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        // 第一步：将已有的 Warm 条目降级为 Cold
        for entry in &mut self.entries {
            if let WorkingEntry::Warm {
                summary,
                token_count,
                source_range,
            } = entry
            {
                let placeholder = format!("[冷缓存] {}", &summary[..summary.len().min(40)]);
                let cold_tokens = (*token_count / 4).max(1);
                *entry = WorkingEntry::Cold {
                    placeholder,
                    source_range: *source_range,
                    token_count: cold_tokens,
                };
            }
        }

        // 第二步：将较旧的 Hot 条目降级为 Warm（保留最近 keep_hot_recent 条为 Hot）
        let hot_cutoff = total.saturating_sub(keep_hot_recent);
        let mut compacted_start = total;
        let mut compacted_end = 0;
        let mut entries_compacted = 0usize;

        for i in 0..hot_cutoff {
            if let WorkingEntry::Hot {
                content,
                token_count,
                ..
            } = &self.entries[i]
            {
                let content = content.clone();
                let token_count = *token_count;

                // 尝试使用 LLM 摘要，失败则回退到截断
                let summary = if let Some(ref summarizer_fn) = summarizer {
                    match summarizer_fn(&content).await {
                        Ok(llm_summary) => llm_summary,
                        Err(_) => {
                            // LLM 摘要失败，回退到截断
                            let truncate_len = (content.len() / 2).max(1);
                            format!("[已压缩] {}...", &content[..truncate_len])
                        }
                    }
                } else {
                    // 没有提供 summarizer，使用截断
                    let truncate_len = (content.len() / 2).max(1);
                    format!("[已压缩] {}...", &content[..truncate_len])
                };

                let warm_tokens = (token_count / 2).max(1);

                self.entries[i] = WorkingEntry::Warm {
                    summary,
                    token_count: warm_tokens,
                    source_range: (i, i),
                };

                compacted_start = compacted_start.min(i);
                compacted_end = compacted_end.max(i);
                entries_compacted += 1;
            }
        }

        self.recalc_used_tokens();

        CompactionResult {
            tokens_before,
            tokens_after: self.used_tokens_cached,
            entries_compacted,
            compacted_range: (compacted_start, compacted_end),
        }
    }

    /// 获取热条目摘要（用于系统提示上下文）
    ///
    /// 返回所有 Hot 条目的简要内容摘要，用于构建系统提示
    pub fn hot_entries_summary(&self) -> String {
        let mut summary = String::new();
        for entry in &self.entries {
            if let WorkingEntry::Hot { role, content, .. } = entry {
                // 每个条目最多取前 100 字符
                let preview = if content.len() > 100 {
                    format!("{}...", &content[..100])
                } else {
                    content.clone()
                };
                summary.push_str(&format!("[{}] {}\n", role, preview));
            }
        }
        summary.trim_end().to_string()
    }

    /// 使用 LLM 生成智能摘要（而非简单截断）
    ///
    /// 摘要请求会发送到 Sampler Node，等待响应后替换原内容。
    /// 如果 LLM 摘要失败，回退到截断方式。
    ///
    /// # Arguments
    /// * `content` - 需要摘要的内容
    ///
    /// # Returns
    /// 摘要后的内容字符串，或截断后备方案
    /// 使用 LLM 生成摘要（如已注入摘要器），否则截断回退
    async fn summarize_with_llm(&self, content: &str) -> Result<String, anyhow::Error> {
        self.summarize_content(content).await
    }

    /// 压缩对话历史（保留工具调用配对）
    ///
    /// 借鉴 Claude Code compaction.rs 的逻辑：
    /// 1. 保留最近 N 轮完整对话（用户+助手+工具调用+结果）
    /// 2. 较早的对话用摘要替换
    /// 3. 工具调用和结果必须配对保留，不可拆开
    pub fn compact_conversation(&mut self, keep_recent_turns: usize) -> Vec<WorkingEntry> {
        let turns = self.group_by_turns();
        let total = turns.len();

        if total <= keep_recent_turns {
            return Vec::new(); // 不需要压缩
        }

        let mut removed = Vec::new();
        let keep_from = total - keep_recent_turns;

        // 收集需要压缩的轮次索引
        let compact_indices: Vec<usize> = (0..keep_from).collect();

        // 从后往前替换，避免索引偏移
        for &turn_start in compact_indices.iter().rev() {
            let turn_end = turns.get(turn_start + 1).copied().unwrap_or(self.entries.len());
            // 将该轮次的条目替换为摘要
            let turn_entries: Vec<&WorkingEntry> = self.entries[turn_start..turn_end].iter().collect();
            let summary = self.summarize_turn(&turn_entries);

            // 移除原条目
            let drained: Vec<WorkingEntry> = self.entries.drain(turn_start..turn_end).collect();
            removed.extend(drained);

            // 插入摘要条目
            let summary_tokens = (summary.len() as u32 / 4).max(1);
            self.entries.insert(turn_start, WorkingEntry::Warm {
                summary,
                token_count: summary_tokens,
                source_range: (turn_start, turn_start),
            });
        }

        removed
    }

    /// 按轮次分组消息
    ///
    /// 每个用户消息开始一个新轮次，后续的助手+工具结果属于同一轮次
    /// 返回每个轮次的起始索引
    fn group_by_turns(&self) -> Vec<usize> {
        let mut turns = Vec::new();

        for (i, entry) in self.entries.iter().enumerate() {
            if let WorkingEntry::Hot { role, .. } = entry {
                if *role == MessageRole::User {
                    turns.push(i);
                }
            }
        }

        turns
    }

    /// 生成轮次摘要
    fn summarize_turn(&self, turn: &[&WorkingEntry]) -> String {
        let contents: Vec<&str> = turn.iter().map(|e| {
            match e {
                WorkingEntry::Hot { content, .. } => content.as_str(),
                WorkingEntry::Warm { summary, .. } => summary.as_str(),
                WorkingEntry::Cold { placeholder, .. } => placeholder.as_str(),
            }
        }).collect();
        let joined = contents.join(" → ");
        let truncated: String = joined.chars().take(200).collect();
        format!("[对话摘要] {}", truncated)
    }

    /// 部分压缩（借鉴 Claude Code partial compaction）
    ///
    /// 只压缩旧消息，保留最近N轮对话完整
    pub fn partial_compact(&mut self, keep_recent_turns: usize) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;
        let turns = self.group_by_turns();
        let total_turns = turns.len();

        if total_turns <= keep_recent_turns {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        let keep_from = total_turns - keep_recent_turns;
        let mut compacted_start = self.entries.len();
        let mut compacted_end = 0;
        let mut entries_compacted = 0usize;

        // 先降级已有的 Warm→Cold
        for entry in &mut self.entries {
            if let WorkingEntry::Warm {
                summary,
                token_count,
                source_range,
            } = entry
            {
                let placeholder = format!("[冷缓存] {}", &summary[..summary.len().min(40)]);
                let cold_tokens = (*token_count / 4).max(1);
                *entry = WorkingEntry::Cold {
                    placeholder,
                    source_range: *source_range,
                    token_count: cold_tokens,
                };
            }
        }

        // 压缩旧轮次（从后往前避免索引偏移）
        for &turn_start in turns[..keep_from].iter().rev() {
            let turn_end = turns.iter().find(|&&t| t > turn_start).copied().unwrap_or(self.entries.len());
            if turn_start >= turn_end {
                continue;
            }

            let turn_entries: Vec<&WorkingEntry> = self.entries[turn_start..turn_end].iter().collect();
            let summary = self.summarize_turn(&turn_entries);

            // 移除原条目
            let drained: Vec<WorkingEntry> = self.entries.drain(turn_start..turn_end).collect();
            entries_compacted += drained.len();
            compacted_start = compacted_start.min(turn_start);
            compacted_end = compacted_end.max(turn_end.min(self.entries.len() + drained.len()));

            // 插入摘要条目
            let summary_tokens = (summary.len() as u32 / 4).max(1);
            self.entries.insert(turn_start, WorkingEntry::Warm {
                summary,
                token_count: summary_tokens,
                source_range: (turn_start, turn_start),
            });
        }

        self.recalc_used_tokens();

        CompactionResult {
            tokens_before,
            tokens_after: self.used_tokens_cached,
            entries_compacted,
            compacted_range: (compacted_start, compacted_end),
        }
    }

    /// 添加 embedding 向量到索引
    ///
    /// # Arguments
    /// * `entry_id` - 条目 ID
    /// * `vector` - Embedding 向量
    /// * `text_preview` - 文本预览（前 100 字符）
    pub fn add_embedding(&mut self, entry_id: String, vector: Vec<f32>, text_preview: String) {
        let embedding = EmbeddingVector {
            data: vector,
            entry_id,
            text_preview,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        self.embedding_index.add(embedding);
    }

    /// 搜索相似条目（基于余弦相似度）
    ///
    /// # Arguments
    /// * `query` - 查询向量
    /// * `k` - 返回结果数量
    ///
    /// # Returns
    /// 返回 entry_id 列表，按相似度降序排列
    pub fn search_similar(&self, query: Vec<f32>, k: usize) -> Vec<String> {
        self.embedding_index
            .search_with_metadata(&query, k)
            .into_iter()
            .map(|(entry_id, _, _)| entry_id)
            .collect()
    }

    /// 使用 MMR 搜索多样化条目
    ///
    /// 平衡相关性和多样性，避免返回过于相似的结果
    ///
    /// # Arguments
    /// * `query` - 查询向量
    /// * `k` - 返回结果数量
    /// * `lambda` - 相关性权重（0.0-1.0），通常 0.5-0.8
    ///
    /// # Returns
    /// 返回 entry_id 列表，按 MMR 分数排序
    pub fn search_diverse(&self, query: Vec<f32>, k: usize, lambda: f32) -> Vec<String> {
        let selected_indices = mmr_select(&self.embedding_index, &query, k, lambda);
        selected_indices
            .into_iter()
            .filter_map(|idx| {
                self.embedding_index
                    .vectors()
                    .get(idx)
                    .map(|v| v.entry_id.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_working_memory_add_embedding() {
        // 创建一个使用 3 维向量的测试索引
        let mut index = EmbeddingIndex::with_dimension(3);

        let embedding = EmbeddingVector {
            data: vec![1.0, 0.0, 0.0],
            entry_id: "test-entry-1".to_string(),
            text_preview: "Test content".to_string(),
            created_at: 1000,
        };

        index.add(embedding);

        // 应该能够通过 entry_id 找到
        assert!(index.find_by_entry_id("test-entry-1").is_some());
    }

    #[test]
    fn test_working_memory_search_similar() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 添加几个 embeddings
        index.add(EmbeddingVector {
            data: vec![1.0, 0.0, 0.0],
            entry_id: "entry-1".to_string(),
            text_preview: "Content 1".to_string(),
            created_at: 1000,
        });
        index.add(EmbeddingVector {
            data: vec![0.0, 1.0, 0.0],
            entry_id: "entry-2".to_string(),
            text_preview: "Content 2".to_string(),
            created_at: 1000,
        });
        index.add(EmbeddingVector {
            data: vec![0.0, 0.0, 1.0],
            entry_id: "entry-3".to_string(),
            text_preview: "Content 3".to_string(),
            created_at: 1000,
        });

        // 搜索与 [1, 0, 0] 最相似的
        let results = index.search_with_metadata(&vec![1.0, 0.0, 0.0], 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "entry-1"); // 应该是第一个，因为完全匹配
    }

    #[test]
    fn test_working_memory_search_diverse() {
        let mut index = EmbeddingIndex::with_dimension(3);

        // 添加几个 embeddings（两个相似，一个不同）
        index.add(EmbeddingVector {
            data: vec![1.0, 0.0, 0.0],
            entry_id: "entry-1".to_string(),
            text_preview: "Content 1".to_string(),
            created_at: 1000,
        });
        index.add(EmbeddingVector {
            data: vec![0.99, 0.1, 0.0],
            entry_id: "entry-2".to_string(),
            text_preview: "Content 2".to_string(),
            created_at: 1000,
        });
        index.add(EmbeddingVector {
            data: vec![0.0, 1.0, 0.0],
            entry_id: "entry-3".to_string(),
            text_preview: "Content 3".to_string(),
            created_at: 1000,
        });

        // 使用 MMR 搜索
        let selected = mmr_select(&index, &vec![1.0, 0.0, 0.0], 2, 0.7);

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0], 0); // 第一个总是最相关的
        // 第二个应该是 1 或 2
        assert!(selected.contains(&1) || selected.contains(&2));
    }

    #[test]
    fn test_working_memory_integration() {
        // 测试 WorkingMemory 的集成
        let mut memory = WorkingMemory::new(10000);

        // 添加一些消息
        memory.push_system("System prompt", 10);
        memory.push_user("User question", 20);
        memory.push_assistant("Assistant response", 30);

        // 验证消息数量
        assert_eq!(memory.entries().len(), 3);
    }
}
