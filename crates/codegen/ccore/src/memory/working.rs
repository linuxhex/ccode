//! L0 工作记忆 - 当前 LLM context window 内的内容
//!
//! 管理当前上下文窗口中的消息，支持 4 级渐进压缩和 Stable Prefix 保护。
//!
//! ## 消息角色（标准 LLM API 格式）
//!
//! - `system`: 系统提示词、工具定义、上下文注入
//! - `user`: 用户输入、感官信号、工具返回结果
//! - `assistant`: LLM 响应、工具调用请求
//!
//! 角色按 system → user → assistant → user → ... 交替。
//!
//! ## 4 级压缩策略（对标 Claude Code）
//!
//! | 级别 | 名称 | 触发条件 | 操作 |
//! |------|------|---------|------|
//! | Snip | 删除长工具输出 | 85%~90% | 截断 >threshold 的工具结果 |
//! | MicroCompact | 单条截断 | 90%~95% | 保留首尾各 retain/2 |
//! | AutoCompact | 批量降级 | 95%~99% | Hot→Warm→Cold |
//! | ReactiveCompact | LLM 全量摘要 | ≥99% | LLM 生成完整摘要替换 |
//!
//! ## Stable Prefix 保护
//!
//! 压缩操作跳过前 `stable_prefix_len` 条条目（system prompt + 工具定义），
//! 最大化 API 侧 Prompt Cache 命中率（类似 Claude Code cache_edits 的替代方案）。
//!
//! ## 增量 Token 计数
//!
//! `used_tokens_cached` 在 push_hot 时递增，compact 后 recalc，
//! 读取 O(1)，避免每次全量遍历。

use serde::{Deserialize, Serialize};
use std::time::Duration;

use super::embedding::{EmbeddingIndex, EmbeddingVector};
use super::mmr::mmr_select;

/// 统一的 token 估算入口
///
/// 委托 `sampler::token_budget::TokenBudgetManager::estimate_tokens`（CJK/ASCII 分权重）。
/// 压缩相关的估算值会汇总进 `used_tokens_cached`，进而决定压缩触发时机——
/// 各处若用不同口径（曾并存 `len()/4` 与 `len()/3`）会让记账口径分叉，
/// 且按字节除法会低估中文，属危险方向（以为还有余量，实际已溢出）。
fn estimate_tokens_u32(text: &str) -> u32 {
    crate::sampler::token_budget::TokenBudgetManager::estimate_tokens(text) as u32
}

use ccode_compaction::cache_aware::{cached_micro_compact, default_fingerprint, CacheAwareState};
use ccode_compaction::micro_compact::{MicroCompactable, MicroCompactConfig};

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

/// 4 级压缩策略（借鉴 Claude Code Snip→MicroCompact→AutoCompact→ReactiveCompact）
///
/// Level 1 (Snip):        删除过长的工具输出，保留关键部分
/// Level 2 (MicroCompact): 单条消息截断，保留首尾 50%
/// Level 3 (AutoCompact):  批量压缩旧消息 Hot→Warm→Cold
/// Level 4 (ReactiveCompact): LLM 全量摘要替换，紧急压缩
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompactionLevel {
    /// Level 1: 删除过长工具输出（最低开销）
    Snip,
    /// Level 2: 单条消息截断
    MicroCompact,
    /// Level 3: 批量降级（Hot→Warm→Cold）
    AutoCompact,
    /// Level 4: LLM 全量摘要（最高开销，最彻底）
    ReactiveCompact,
}

/// 上下文压缩策略（借鉴 Claude Code CompactionPolicy + cache 前缀稳定化）
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
    /// 是否启用 system prompt 前缀稳定化（类似 Claude Code cache_edits）
    ///
    /// 开启时，压缩操作不会修改 system prompt 前缀部分，
    /// 而是在前缀之后插入压缩摘要，最大化 API 侧 Prompt Cache 命中率。
    pub stable_prefix_enabled: bool,
    /// 工具输出截断阈值（Level 1 Snip 触发条件）
    ///
    /// 超过此 token 数的工具输出将被截断为摘要
    pub tool_output_snip_threshold: u32,
    /// 单条消息截断比例（Level 2 MicroCompact）
    ///
    /// 0.5 表示保留首尾各 25%，中间替换为 [...已省略...]
    pub micro_compact_retain_ratio: f64,
    /// 最大连续压缩失败次数（超过后停止自动压缩，类似断路器）
    ///
    /// 借鉴 Claude Code MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES = 3
    pub max_consecutive_compact_failures: u32,
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
            stable_prefix_enabled: true,
            tool_output_snip_threshold: 2000,
            micro_compact_retain_ratio: 0.5,
            max_consecutive_compact_failures: 3,
        }
    }
}

impl From<&CompactionPolicy> for MicroCompactConfig {
    fn from(policy: &CompactionPolicy) -> Self {
        // micro_compact_retain_ratio 用于控制保留比例，但 MicroCompactConfig
        // 使用 summary_max_chars（绝对字符数）。将 ratio 映射到 200~600 字符范围。
        let summary_max_chars = (200.0 + policy.micro_compact_retain_ratio * 400.0) as usize;
        Self {
            max_age: Duration::from_secs(policy.wall_clock_budget_secs.max(60)),
            summary_max_chars,
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

    /// 根据使用率选择压缩级别
    ///
    /// - 85%~90%: Snip (删除长工具输出)
    /// - 90%~95%: MicroCompact (单条截断)
    /// - 95%~99%: AutoCompact (批量降级)
    /// - >=99%:   ReactiveCompact (LLM 全量摘要)
    pub fn select_level(&self, token_usage_ratio: f64) -> CompactionLevel {
        if token_usage_ratio >= 0.99 {
            CompactionLevel::ReactiveCompact
        } else if token_usage_ratio >= 0.95 {
            CompactionLevel::AutoCompact
        } else if token_usage_ratio >= 0.90 {
            CompactionLevel::MicroCompact
        } else {
            CompactionLevel::Snip
        }
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
        /// 原始工具名（仅工具结果条目有值，用于微压缩指纹区分）
        tool_name: Option<String>,
        /// 创建时间（用于微压缩的年龄判断；不参与序列化）
        #[serde(skip, default = "default_created_at")]
        created_at: std::time::Instant,
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

/// 为反序列化的 Hot 条目提供默认的创建时间（视为"新条目"）
fn default_created_at() -> std::time::Instant {
    std::time::Instant::now()
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

// ============================================================================
// MicroCompactable 实现 — 接入 ccode-compaction 的微压缩层
// ============================================================================

impl MicroCompactable for WorkingEntry {
    fn created_at(&self) -> std::time::Instant {
        match self {
            // Hot 条目返回真实创建时间，用于年龄判断
            Self::Hot { created_at, .. } => *created_at,
            // Warm/Cold 已是压缩态，视为"新鲜"（不会被微压缩处理）
            _ => std::time::Instant::now(),
        }
    }

    fn is_tool_result(&self) -> bool {
        // ccore 约定：工具结果存为 Hot + role=User
        matches!(self, Self::Hot { role: MessageRole::User, .. })
    }

    fn tool_name(&self) -> Option<&str> {
        match self {
            Self::Hot {
                role: MessageRole::User,
                tool_name: Some(ref name),
                ..
            } => Some(name.as_str()),
            // Warm/Cold 已是压缩态，不参与微压缩处理
            _ => None,
        }
    }

    fn content(&self) -> &str {
        match self {
            Self::Hot { content, .. } => content.as_str(),
            Self::Warm { summary, .. } => summary.as_str(),
            Self::Cold { placeholder, .. } => placeholder.as_str(),
        }
    }

    fn with_content(&self, new_content: String) -> Self {
        match self {
            Self::Hot {
                role,
                token_count,
                tool_name,
                created_at,
                ..
            } => {
                // 按新内容估算 token，至少 1。
                // 复用统一估算器（CJK/ASCII 分权重）——按字节 len()/4 会低估中文，
                // 而这些值会汇总进 used_tokens_cached 决定压缩触发时机，低估会导致
                // 上下文实际溢出但记账以为还有余量。
                let new_tokens = estimate_tokens_u32(&new_content).max(1);
                Self::Hot {
                    role: *role,
                    content: new_content,
                    token_count: new_tokens.min(*token_count),
                    tool_name: tool_name.clone(),
                    created_at: *created_at,
                }
            }
            // Warm/Cold 不参与微压缩内容替换，原样返回
            _ => self.clone(),
        }
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
    /// Stable prefix 条目数（类似 Claude Code cache_edits 前缀稳定化）
    ///
    /// 前缀部分（system prompt + 工具定义）在压缩时不会被修改，
    /// 压缩操作只影响前缀之后的条目，最大化 API 侧 Prompt Cache 命中率。
    /// 在首次 push_system 后锁定，后续 push 操作不增加前缀长度。
    stable_prefix_len: usize,
    /// Cache-Aware 压缩状态（追踪已压缩内容指纹，避免重复压缩）
    cache_state: CacheAwareState,
    /// 连续压缩失败次数（断路器，超过阈值后停止自动压缩）
    consecutive_compact_failures: u32,
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
            stable_prefix_len: 0,
            cache_state: CacheAwareState::default(),
            consecutive_compact_failures: 0,
        }
    }

    /// 锁定 stable prefix（在 system prompt + 工具定义推送完成后调用）
    ///
    /// 之后的压缩操作不会修改前缀部分，最大化 API Prompt Cache 命中率。
    pub fn lock_stable_prefix(&mut self) {
        self.stable_prefix_len = self.entries.len();
        tracing::debug!(
            target: "ccore::memory",
            prefix_len = self.stable_prefix_len,
            "stable prefix locked"
        );
    }

    /// 获取 stable prefix 长度
    pub fn stable_prefix_len(&self) -> usize {
        self.stable_prefix_len
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

    /// Level 4 异步路径：真正的 LLM 全量摘要替换
    ///
    /// 收集 stable prefix 之后的所有条目，调用 LLM 生成摘要，
    /// 然后用摘要替换这些条目。如果 LLM 不可用或失败，回退到同步的 AutoCompact。
    ///
    /// 返回压缩结果。调用方应在 `compact()` 返回 ReactiveCompact 级别后调用此方法。
    pub async fn compact_reactive_llm(&mut self) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;
        let prefix_end = if self.compaction_policy.stable_prefix_enabled { self.stable_prefix_len } else { 0 };

        if prefix_end >= self.entries.len() {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        // 收集需要摘要的内容
        let mut content_parts = Vec::new();
        for entry in &self.entries[prefix_end..] {
            match entry {
                WorkingEntry::Hot { content, .. } => content_parts.push(content.clone()),
                WorkingEntry::Warm { summary, .. } => content_parts.push(summary.clone()),
                WorkingEntry::Cold { placeholder, .. } => content_parts.push(placeholder.clone()),
            }
        }

        if content_parts.is_empty() {
            return CompactionResult {
                tokens_before,
                tokens_after: tokens_before,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        let full_content = content_parts.join("\n---\n");
        let summary_prompt = concat!(
            "请将以下对话历史压缩为简洁摘要。保留：关键决策、文件路径、错误修复、",
            "用户意图、未完成任务。丢弃：闲聊、重复内容、原始工具输出。\n\n"
        );

        let summary = match self.summarize_content(&format!("{}\n{}", summary_prompt, full_content)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("ReactiveCompact LLM 摘要失败: {}，回退到 AutoCompact", e);
                let policy = self.compaction_policy.clone();
                return self.compact_with_policy(&policy);
            }
        };

        // 计算摘要 token 数
        let summary_tokens = (summary.len() as f32 / 4.0) as u32;

        // 移除 prefix_end 之后的所有条目，替换为摘要
        let removed_count = self.entries.len() - prefix_end;
        self.entries.truncate(prefix_end);
        self.entries.push(WorkingEntry::Hot {
            role: MessageRole::System,
            content: format!("<conversation_summary>\n{}\n</conversation_summary>", summary),
            token_count: summary_tokens,
            tool_name: None,
            created_at: std::time::Instant::now(),
        });

        // 重新计算 token 使用量
        self.recalculate_tokens();

        let tokens_after = self.used_tokens_cached;
        tracing::info!(
            tokens_before,
            tokens_after,
            entries_removed = removed_count,
            "ReactiveCompact: LLM 摘要替换完成"
        );

        CompactionResult {
            tokens_before,
            tokens_after,
            entries_compacted: removed_count,
            compacted_range: (prefix_end, prefix_end + removed_count),
        }
    }

    /// 重新计算 token 使用量（遍历所有条目）
    fn recalculate_tokens(&mut self) {
        self.used_tokens_cached = self.entries.iter().map(|e| e.token_count()).sum::<u32>();
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

    /// 添加工具结果消息（显式角色 + 工具名）
    pub fn push_hot_with_tool(
        &mut self,
        role: MessageRole,
        content: String,
        token_count: u32,
        tool_name: Option<String>,
    ) {
        self.used_tokens_cached += token_count;
        self.entries.push(WorkingEntry::Hot {
            role,
            content,
            token_count,
            tool_name,
            created_at: std::time::Instant::now(),
        });
    }

    /// 添加热消息（显式角色）
    pub fn push_hot(&mut self, role: MessageRole, content: String, token_count: u32) {
        self.push_hot_with_tool(role, content, token_count, None);
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

    /// 判断是否需要压缩（含断路器：连续失败超过阈值后停止自动压缩）
    pub fn should_compact(&self) -> bool {
        if self.max_tokens == 0 {
            return false;
        }
        // 断路器：连续压缩失败次数超过阈值则停止自动压缩
        if self.consecutive_compact_failures >= self.compaction_policy.max_consecutive_compact_failures {
            return false;
        }
        let usage_percent = (self.used_tokens_cached as u64 * 100) / self.max_tokens as u64;
        usage_percent >= self.compaction_policy.auto_compact_threshold_percent as u64
    }

    /// 使用当前策略执行压缩（含断路器计数）
    pub fn compact(&mut self) -> CompactionResult {
        let result = self.compact_with_policy(&self.compaction_policy.clone());
        // 断路器：压缩后 token 未减少则视为失败，超过阈值后停止自动压缩
        if result.tokens_after >= result.tokens_before {
            self.consecutive_compact_failures += 1;
            tracing::warn!(
                consecutive_failures = self.consecutive_compact_failures,
                max_failures = self.compaction_policy.max_consecutive_compact_failures,
                "compact: no tokens saved, incrementing failure counter"
            );
        } else {
            self.consecutive_compact_failures = 0;
        }
        result
    }

    /// 使用指定策略执行压缩（同步版本，4 级策略 + stable prefix 保护）
    ///
    /// 策略（根据 token 使用率自动选择级别）：
    /// - Level 1 (Snip):         删除过长工具输出（>threshold token 的工具结果截断）
    /// - Level 2 (MicroCompact): 单条消息首尾截断
    /// - Level 3 (AutoCompact):  批量降级 Hot→Warm→Cold
    /// - Level 4 (ReactiveCompact): LLM 全量摘要
    ///
    /// Stable prefix 保护：
    /// - 如果 stable_prefix_enabled，压缩操作跳过前 stable_prefix_len 条条目
    /// - 这些条目（system prompt + 工具定义）保持不变，最大化 Prompt Cache 命中率
    pub fn compact_with_policy(&mut self, policy: &CompactionPolicy) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;

        // 根据使用率选择压缩级别
        let usage_ratio = if self.max_tokens > 0 {
            self.used_tokens_cached as f64 / self.max_tokens as f64
        } else {
            0.0
        };
        let level = policy.select_level(usage_ratio);

        // Stable prefix 保护：压缩只影响前缀之后的条目
        let prefix_end = if policy.stable_prefix_enabled {
            self.stable_prefix_len
        } else {
            0
        };

        match level {
            CompactionLevel::Snip => self.compact_snip(policy, prefix_end),
            CompactionLevel::MicroCompact => {
                // 先 Snip，再 MicroCompact
                let r1 = self.compact_snip(policy, prefix_end);
                if self.used_tokens_cached as f64 / self.max_tokens.max(1) as f64 >= 0.90 {
                    let r2 = self.compact_micro(policy, prefix_end);
                    CompactionResult {
                        tokens_before,
                        tokens_after: r2.tokens_after,
                        entries_compacted: r1.entries_compacted + r2.entries_compacted,
                        compacted_range: (
                            r1.compacted_range.0.min(r2.compacted_range.0),
                            r1.compacted_range.1.max(r2.compacted_range.1),
                        ),
                    }
                } else {
                    r1
                }
            }
            CompactionLevel::AutoCompact => {
                // Snip + MicroCompact + AutoCompact
                self.compact_snip(policy, prefix_end);
                self.compact_micro(policy, prefix_end);
                self.compact_auto(policy, prefix_end, tokens_before)
            }
            CompactionLevel::ReactiveCompact => {
                // 全量：Snip + MicroCompact + AutoCompact（最激进）
                self.compact_snip(policy, prefix_end);
                self.compact_micro(policy, prefix_end);
                self.compact_auto(policy, prefix_end, tokens_before)
            }
        }
    }

    /// Level 1: Snip — 截断过长的工具输出
    ///
    /// 只处理 role=User 且 token_count 超过阈值的条目（通常是工具返回结果），
    /// 截断为摘要格式，保留首尾各 500 字符。
    fn compact_snip(&mut self, policy: &CompactionPolicy, prefix_end: usize) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;
        let mut entries_compacted = 0usize;
        let threshold = policy.tool_output_snip_threshold;

        for i in prefix_end..self.entries.len() {
            let entry = std::mem::replace(&mut self.entries[i], WorkingEntry::Cold {
                placeholder: String::new(),
                source_range: (i, i),
                token_count: 0,
            });

            if let WorkingEntry::Hot {
                role: MessageRole::User,
                content,
                token_count,
                tool_name,
                created_at,
            } = &entry
            {
                if *token_count > threshold {
                    // 截断：保留首尾各 500 字符
                    let head_len = content.len().min(500);
                    let tail_start = content.len().saturating_sub(500);
                    let head = &content[..head_len];
                    let tail = &content[tail_start..];
                    let snipped = if tail_start > head_len {
                        format!("{}\n[...已省略 {} 字符...]\n{}", head, tail_start - head_len, tail)
                    } else {
                        content.clone()
                    };
                    // 截断后按实际保留内容重新估算，而不是按原 token_count/3 猜。
                    // 猜算法在保留 1000 字符时与实际不符，且这些值汇总进
                    // used_tokens_cached 决定后续压缩级别选择，口径必须真实。
                    let snipped_tokens = estimate_tokens_u32(&snipped).max(1);
                    self.entries[i] = WorkingEntry::Hot {
                        role: MessageRole::User,
                        content: snipped,
                        token_count: snipped_tokens,
                        tool_name: tool_name.clone(),
                        created_at: *created_at,
                    };
                    entries_compacted += 1;
                } else {
                    self.entries[i] = entry;
                }
            } else {
                self.entries[i] = entry;
            }
        }

        self.recalc_used_tokens();
        CompactionResult {
            tokens_before,
            tokens_after: self.used_tokens_cached,
            entries_compacted,
            compacted_range: (prefix_end, self.entries.len()),
        }
    }

    /// Level 2: MicroCompact — 按年龄清除超龄工具结果
    ///
    /// 委托给 `ccode_compaction::micro_compact_messages`：对前缀之后的条目，
    /// 超过 `max_age` 的可压缩工具结果会被替换为清除标记或截断摘要。
    /// 相比原先的首尾截断，此实现按时间年龄清除，更贴近 Claude Code 的微压缩语义。
    fn compact_micro(&mut self, policy: &CompactionPolicy, prefix_end: usize) -> CompactionResult {
        let tokens_before = self.used_tokens_cached;
        let config = MicroCompactConfig::from(policy);
        let now = std::time::Instant::now();

        let prefix_len = prefix_end.min(self.entries.len());
        let mut all_entries = std::mem::take(&mut self.entries);
        // split_off 之后 all_entries 保留前 prefix_len 条，rest 是剩余部分
        let rest = all_entries.split_off(prefix_len);

        // 记录原始内容用于统计变更数
        let old_contents: Vec<String> =
            rest.iter().map(|e| e.content().to_string()).collect();

        // 使用 cache-aware 微压缩：跳过已压缩内容的指纹匹配
        let result = cached_micro_compact::<WorkingEntry>(
            &rest,
            &config,
            now,
            &mut self.cache_state,
            default_fingerprint::<WorkingEntry>,
        );

        let entries_compacted = old_contents
            .iter()
            .zip(result.items.iter())
            .filter(|(old, new)| old.as_str() != new.content())
            .count();

        all_entries.extend(result.items);
        self.entries = all_entries;

        self.recalc_used_tokens();
        CompactionResult {
            tokens_before,
            tokens_after: self.used_tokens_cached,
            entries_compacted,
            compacted_range: (prefix_end, self.entries.len()),
        }
    }

    /// Level 3: AutoCompact — 批量降级 Hot→Warm→Cold
    ///
    /// 与原 compact_with_policy 逻辑相同，但尊重 stable prefix 边界。
    fn compact_auto(&mut self, _policy: &CompactionPolicy, prefix_end: usize, tokens_before: u32) -> CompactionResult {
        let total = self.entries.len();
        let keep_hot_recent = 4;

        if total <= prefix_end + keep_hot_recent {
            return CompactionResult {
                tokens_before,
                tokens_after: self.used_tokens_cached,
                entries_compacted: 0,
                compacted_range: (0, 0),
            };
        }

        // 第一步：将已有的 Warm 条目降级为 Cold（跳过 prefix）
        for i in prefix_end..total {
            if let WorkingEntry::Warm {
                summary,
                token_count,
                source_range,
            } = &self.entries[i]
            {
                let bound = summary.floor_char_boundary(40);
                let placeholder = format!("[冷缓存] {}", &summary[..bound]);
                let cold_tokens = (token_count / 4).max(1);
                self.entries[i] = WorkingEntry::Cold {
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

        // 从 prefix_end 开始，不压缩 stable prefix
        for i in prefix_end..hot_cutoff {
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

    /// 获取热条目摘要（用于系统提示上下文）
    ///
    /// 返回所有 Hot 条目的简要内容摘要，用于构建系统提示
    pub fn hot_entries_summary(&self) -> String {
        let mut summary = String::new();
        for entry in &self.entries {
            if let WorkingEntry::Hot { role, content, .. } = entry {
                // 每个条目最多取前 100 字符（按字符边界切，中文内容按字节切会 panic）
                let preview = if content.len() > 100 {
                    let bound = content.floor_char_boundary(100);
                    format!("{}...", &content[..bound])
                } else {
                    content.clone()
                };
                summary.push_str(&format!("[{}] {}\n", role, preview));
            }
        }
        summary.trim_end().to_string()
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
            let summary_tokens = estimate_tokens_u32(&summary).max(1);
            self.entries.insert(turn_start, WorkingEntry::Warm {
                summary,
                token_count: summary_tokens,
                // 记录真实跨度而非 (turn_start, turn_start)：该字段会被
                // window.rs 渲染成用户可见的 `[冷记忆: 第X-Y轮]`，
                // 记成同一轮会让用户看到 "第5-5轮" 这种错误提示
                source_range: (turn_start, turn_end),
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
        // 注意：search 过滤 score <= 0.0，正交向量相似度为 0 被过滤，
        // 仅返回 entry-1（与 query 完全匹配）
        let results = index.search_with_metadata(&[1.0, 0.0, 0.0], 2);

        assert_eq!(results.len(), 1);
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
        let selected = mmr_select(&index, &[1.0, 0.0, 0.0], 2, 0.7);

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

    /// 压缩相关的 token 估算必须与全局统一口径一致。
    ///
    /// 这些值汇总进 `used_tokens_cached`，决定压缩触发时机与级别选择。
    /// 此前并存 `len()/4`（3 处）与 `len()/3`（1 处）两种按字节除法，
    /// 口径分叉且都低估中文——低估是危险方向（记账以为有余量，实际已溢出）。
    #[test]
    fn estimate_tokens_matches_global_estimator() {
        for s in ["", "a", "中", "调用 read_file 失败", "pub fn main() {}"] {
            assert_eq!(
                estimate_tokens_u32(s) as usize,
                crate::sampler::token_budget::TokenBudgetManager::estimate_tokens(s),
                "working.rs 的估算应与 TokenBudgetManager 一致（输入：{s:?}）"
            );
        }
    }

    /// 中文内容不得触发字节切片 panic。
    #[test]
    fn hot_entries_summary_handles_cjk_without_panic() {
        let mut memory = WorkingMemory::new(10000);
        // 超过 100 字节的纯中文（每字 3 字节，40 字 = 120 字节）
        let long_cjk = "检测到工具重复调用请换一种方法避免重复相同的操作以免陷入循环浪费预算".repeat(2);
        assert!(long_cjk.len() > 100, "测试内容需超过 100 字节");

        let tokens = estimate_tokens_u32(&long_cjk);
        memory.push_user(long_cjk, tokens);

        // 不 panic 即通过；同时确认确实做了截断
        let summary = memory.hot_entries_summary();
        assert!(summary.contains("..."), "超长内容应被截断");
    }

    /// 压缩后的 Warm/Cold 条目序列化为 system 角色——这是 provider 侧
    /// `extract_system_messages` 能正确提取的前提。若改成 user/assistant，
    /// 摘要就会混进 messages 参与角色交替，需同步调整 provider 逻辑。
    #[test]
    fn compacted_entries_serialize_as_system_role() {
        let mut memory = WorkingMemory::new(10000);
        memory.push_user("问题", 10);
        memory.replace_entries(vec![
            WorkingEntry::Warm {
                summary: "早期对话摘要".into(),
                token_count: 5,
                source_range: (0, 2),
            },
            WorkingEntry::Cold {
                placeholder: "[冷记忆]".into(),
                token_count: 3,
                source_range: (2, 4),
            },
        ]);

        let msgs = memory.to_chat_messages();
        assert_eq!(msgs.len(), 2);
        for (role, _) in &msgs {
            assert_eq!(
                role, "system",
                "Warm/Cold 必须序列化为 system，否则 provider 侧提取逻辑失效"
            );
        }
    }

    /// 压缩后仍须能构成合法的 Anthropic 请求。
    ///
    /// `compact_auto` 按**条目数**保留最近 4 条，而一个 turn
    /// （user + assistant + N 个工具结果）轻易超过 4 条，故可能把 assistant
    /// 与其工具结果拆开。本测试实测拆开后经 provider 侧处理是否仍然合法。
    #[test]
    fn compaction_preserves_valid_conversation_shape() {
        use crate::sampler::claude_compat::ClaudeCompatProvider;

        let mut memory = WorkingMemory::new(200);
        // 两个完整 turn 共 10 条，远超 keep_hot_recent=4
        for turn in 0..2 {
            memory.push_user(format!("问题{turn}"), 30);
            memory.push_assistant(format!("[调用工具：read_file, grep]{turn}"), 30);
            memory.push_user(format!("工具结果A{turn}"), 30);
            memory.push_user(format!("工具结果B{turn}"), 30);
            memory.push_user(format!("工具结果C{turn}"), 30);
        }

        let result = memory.compact();
        assert!(
            result.tokens_after < result.tokens_before,
            "压缩应减少 token：{} → {}",
            result.tokens_before,
            result.tokens_after
        );

        // 压缩产物经 provider 处理后必须严格交替
        let msgs = memory.to_chat_messages_direct(|role, content| {
            crate::sampler::provider::ChatMessage { role, content, cache_control: None }
        });
        let (_system, rest) = ClaudeCompatProvider::extract_system_messages(&msgs);
        let merged = ClaudeCompatProvider::merge_consecutive_roles(&rest);

        for pair in merged.windows(2) {
            assert_ne!(
                pair[0].role, pair[1].role,
                "压缩后经 provider 处理仍须严格交替，实际：{:?}",
                merged.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
            );
        }
    }

    /// 按轮次压缩保留最近 N 轮完整，且真实减少 token。
    ///
    /// 该函数此前零调用（注释宣称「工具调用与结果配对保留」但从未执行），
    /// 现已接入 `run_compaction_pipeline` 作为 L3 最终兜底。
    #[test]
    fn partial_compact_keeps_recent_turns_and_saves_tokens() {
        let mut memory = WorkingMemory::new(10_000);
        // 4 个轮次，每轮 3 条
        for turn in 0..4 {
            memory.push_user(format!("问题{turn}"), 50);
            memory.push_assistant(format!("回答{turn}"), 50);
            memory.push_user(format!("工具结果{turn}"), 50);
        }
        let before = memory.used_tokens();

        let result = memory.partial_compact(2);

        assert!(
            result.tokens_after < before,
            "按轮次压缩应减少 token：{before} → {}",
            result.tokens_after
        );
        assert!(result.entries_compacted > 0, "应有条目被压缩");

        // 最近 2 轮的内容应仍以 Hot 形式存在
        let msgs = memory.to_chat_messages();
        let joined = msgs
            .iter()
            .map(|(_, c)| c.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(joined.contains("问题3"), "最近轮次应保留完整");
        assert!(joined.contains("问题2"), "最近 2 轮都应保留");
    }

    /// `source_range` 必须记录真实跨度——该字段被 window.rs 渲染成
    /// 用户可见的 `[冷记忆: 第X-Y轮]`，记成 (start, start) 会显示 "第5-5轮"。
    #[test]
    fn partial_compact_records_real_source_range() {
        let mut memory = WorkingMemory::new(10_000);
        for turn in 0..4 {
            memory.push_user(format!("问题{turn}"), 50);
            memory.push_assistant(format!("回答{turn}"), 50);
            memory.push_user(format!("工具结果{turn}"), 50);
        }

        memory.partial_compact(2);

        let warm_ranges: Vec<(usize, usize)> = memory
            .entries()
            .iter()
            .filter_map(|e| match e {
                WorkingEntry::Warm { source_range, .. } => Some(*source_range),
                _ => None,
            })
            .collect();

        assert!(!warm_ranges.is_empty(), "应产生 Warm 摘要条目");
        for (start, end) in warm_ranges {
            assert!(
                end > start,
                "source_range 应记录真实跨度，实得 ({start}, {end})"
            );
        }
    }
}
