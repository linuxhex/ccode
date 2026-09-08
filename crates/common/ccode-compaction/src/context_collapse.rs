//! Context Collapse — 大范围 LLM 摘要压缩。
//!
//! 当 MicroCompact + Snip + Budget 之后仍超 token 预算，对早期轮次做
//! LLM 摘要：保留关键决策、错误纠正、约束条件、已排除方案；丢弃闲聊、
//! 试错、重复。这是 5 层压缩管道的最后一层（Auto-Compact 触发后执行）。
//!
//! 对标 Claude Code 的 Context Collapse。LLM 调用通过 [`CollapseSampler`]
//! trait 注入，本模块不依赖具体 sampling crate。

use crate::item::CompactionItem;

/// Context Collapse 配置。
#[derive(Debug, Clone)]
pub struct ContextCollapseConfig {
    /// 触发阈值：token 占用超过 context_window 的此百分比时触发。
    pub collapse_threshold_percent: u32,
    /// 摘要的 token 预算上限。
    pub max_summary_tokens: u32,
    /// 保留最近 K 轮不压缩（热区）。
    pub keep_recent: usize,
}

impl Default for ContextCollapseConfig {
    fn default() -> Self {
        Self {
            collapse_threshold_percent: 85,
            max_summary_tokens: 2000,
            keep_recent: 6,
        }
    }
}

/// 是否应触发 context collapse。
pub fn should_collapse(total_tokens: u64, context_window: u64, config: &ContextCollapseConfig) -> bool {
    if context_window == 0 {
        return false;
    }
    let pct = (total_tokens * 100) / context_window;
    pct >= config.collapse_threshold_percent as u64
}

/// LLM 摘要调用 seam（宿主注入真实 sampler）。
#[async_trait::async_trait]
pub trait CollapseSampler: Send + Sync {
    /// 对 `to_summarize` 生成摘要，不超过 `max_tokens`。
    async fn summarize(&self, to_summarize: &str, max_tokens: u32) -> Result<String, String>;
}

/// Collapse 错误。
#[derive(Debug, thiserror::Error)]
pub enum CollapseError {
    #[error("sampler failed: {0}")]
    Sampler(String),
    #[error("degenerate summary")]
    Degenerate,
}

/// Collapse 结果。
#[derive(Debug, Clone)]
pub struct CollapseResult<T> {
    /// 摘要文本（作为前置 carrier）+ 保留的最近轮次。
    pub items: Vec<T>,
    /// 生成的摘要文本。
    pub summary: String,
    /// 被压缩的轮次数。
    pub collapsed_count: usize,
}

/// 对 `items` 执行 context collapse。
///
/// 1. 把 `keep_recent` 之后的早期轮次拼成文本。
/// 2. 调用 `sampler.summarize` 生成摘要。
/// 3. 用 `build_summary_carrier` 把摘要包成一条 carrier item 放到最前。
/// 4. 返回 carrier + 保留的最近轮次。
///
/// `flatten` 由宿主提供，把早期轮次拼成给 LLM 的纯文本。
/// `build_summary_carrier` 由宿主提供，把摘要文本包回一个
/// `is_compaction_summary() == true` 的 item。
pub async fn context_collapse<T, F, C>(
    items: &[T],
    config: &ContextCollapseConfig,
    sampler: &dyn CollapseSampler,
    flatten: F,
    build_summary_carrier: C,
) -> Result<CollapseResult<T>, CollapseError>
where
    T: CompactionItem + Clone,
    F: Fn(&[&T]) -> String,
    C: Fn(String) -> T,
{
    if items.len() <= config.keep_recent {
        return Ok(CollapseResult {
            items: items.to_vec(),
            summary: String::new(),
            collapsed_count: 0,
        });
    }
    let split = items.len() - config.keep_recent;
    let early: Vec<&T> = items[..split].iter().collect();
    let recent: Vec<&T> = items[split..].iter().collect();

    let to_summarize = flatten(&early);
    if to_summarize.trim().is_empty() {
        return Ok(CollapseResult {
            items: items.to_vec(),
            summary: String::new(),
            collapsed_count: 0,
        });
    }
    let summary = sampler
        .summarize(&to_summarize, config.max_summary_tokens)
        .await
        .map_err(CollapseError::Sampler)?;
    if is_degenerate(&summary) {
        return Err(CollapseError::Degenerate);
    }
    let carrier = build_summary_carrier(summary.clone());
    let mut out = Vec::with_capacity(1 + recent.len());
    out.push(carrier);
    out.extend(recent.iter().map(|i| (*i).clone()));
    Ok(CollapseResult {
        items: out,
        summary,
        collapsed_count: early.len(),
    })
}

/// 摘要退化检测：过短或全是占位符的摘要视为退化。
pub fn is_degenerate(summary: &str) -> bool {
    let trimmed = summary.trim();
    trimmed.len() < 20 || trimmed.chars().all(|c| c == '.' || c == ' ')
}

/// 压缩质量评分（0.0~1.0）
///
/// 检查摘要是否保留了原始内容中的关键信息：
/// - 文件路径（以 `/` 或 `.` 开头，含扩展名）
/// - 错误信息（`error`、`failed`、`panic` 关键词）
/// - 代码标识符（`fn `、`struct `、`impl `、`let `）
/// - 用户意图（`TODO`、`FIXME`、任务描述）
///
/// 返回保留比例。低于 0.3 表示压缩丢失了过多关键信息。
pub fn compaction_quality_score(original: &str, summary: &str) -> f64 {
    let summary_lower = summary.to_lowercase();
    let original_lower = original.to_lowercase();

    let mut total_signals = 0u32;
    let mut preserved_signals = 0u32;

    // 检查文件路径模式
    for word in original_lower.split_whitespace() {
        if (word.starts_with("./") || word.starts_with("/"))
            && word.contains('.')
            && word.len() > 3
            && word.len() < 200
        {
            total_signals += 1;
            if summary_lower.contains(word) {
                preserved_signals += 1;
            }
        }
    }

    // 检查错误关键词
    let error_keywords = ["error", "failed", "panic", "unwrap", "exception"];
    for kw in &error_keywords {
        if original_lower.contains(kw) {
            total_signals += 1;
            if summary_lower.contains(kw) {
                preserved_signals += 1;
            }
        }
    }

    // 检查代码标识符
    let code_patterns = ["fn ", "struct ", "impl ", "let ", "pub ", "async "];
    for pat in &code_patterns {
        if original_lower.contains(pat) {
            total_signals += 1;
            if summary_lower.contains(pat) {
                preserved_signals += 1;
            }
        }
    }

    // 检查任务标记
    let task_markers = ["todo", "fixme", "hack", "xxx"];
    for marker in &task_markers {
        if original_lower.contains(marker) {
            total_signals += 1;
            if summary_lower.contains(marker) {
                preserved_signals += 1;
            }
        }
    }

    if total_signals == 0 {
        // 无可检测信号时，基于长度比给出中等分数
        let len_ratio = summary.len() as f64 / original.len().max(1) as f64;
        return if len_ratio > 0.05 && len_ratio < 0.5 { 0.7 } else { 0.5 };
    }

    preserved_signals as f64 / total_signals as f64
}

/// 摘要 prompt 模板：指示 LLM 保留决策/纠正/约束、丢弃闲聊/试错。
pub const COLLAPSE_PROMPT: &str = "\
Summarize the following earlier conversation turns for context continuity.
PRESERVE: key decisions, error corrections, constraints, ruled-out approaches, project facts.
DROP: chitchat, trial-and-error, repeated content, raw tool output.
Be concise and factual. Output only the summary.";

/// 增量摘要 prompt 模板 — 基于前次摘要 + 新增轮次生成更新摘要
pub const INCREMENTAL_COLLAPSE_PROMPT: &str = "\
You are given a previous conversation summary and new turns that occurred since.
Produce an updated summary that incorporates the new information while preserving \
all still-relevant information from the previous summary.

RULES:
- If the previous summary is marked with <conversation_summary> tags, treat it as \
  authoritative for early history and carry its still-relevant information forward.
- PRESERVE: key decisions, error corrections, constraints, ruled-out approaches, \
  file paths, project facts, pending tasks.
- DROP: information that is no longer relevant after the new turns.
- Be concise and factual. Output only the updated summary.";

/// 增量摘要 — 仅摘要新增轮次 + 前次摘要，而非重新摘要全部对话
///
/// 比全量摘要更 token 高效，且产生更稳定的结果（前次摘要中的信息不会丢失）。
///
/// # Arguments
/// * `sampler` - LLM 摘要器
/// * `previous_summary` - 前次压缩产生的摘要
/// * `new_content` - 自上次压缩以来的新增内容（已格式化为文本）
/// * `config` - 压缩配置
///
/// # Returns
/// 更新后的摘要文本
pub async fn incremental_summary<S: CollapseSampler>(
    sampler: &S,
    previous_summary: &str,
    new_content: &str,
    config: &ContextCollapseConfig,
) -> Result<String, CollapseError> {
    let input = format!(
        "{}\n\n<previous_summary>\n{}\n</previous_summary>\n\n<new_turns>\n{}\n</new_turns>",
        INCREMENTAL_COLLAPSE_PROMPT, previous_summary, new_content
    );

    let summary = sampler.summarize(&input, config.max_summary_tokens).await
        .map_err(|e| CollapseError::Sampler(e))?;

    if is_degenerate(&summary) {
        return Err(CollapseError::Degenerate);
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::CompactionRole;

    #[test]
    fn threshold_check() {
        let cfg = ContextCollapseConfig::default();
        assert!(!should_collapse(80_000, 100_000, &cfg));
        assert!(should_collapse(86_000, 100_000, &cfg));
        assert!(!should_collapse(50, 0, &cfg));
    }

    struct OkSampler;
    #[async_trait::async_trait]
    impl CollapseSampler for OkSampler {
        async fn summarize(&self, _: &str, _: u32) -> Result<String, String> {
            Ok("Decided to use the bridge pattern. Ruled out direct integration.".into())
        }
    }

    #[derive(Clone)]
    struct Item(String);
    impl CompactionItem for Item {
        fn role(&self) -> CompactionRole {
            CompactionRole::User
        }
        fn text(&self) -> Option<String> {
            Some(self.0.clone())
        }
        fn has_tool_requests(&self) -> bool {
            false
        }
        fn is_compaction_summary(&self) -> bool {
            false
        }
        fn attachment_refs(&self) -> Vec<crate::item::CompactionFileRef> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn collapses_early_turns() {
        let cfg = ContextCollapseConfig {
            keep_recent: 2,
            ..Default::default()
        };
        let items: Vec<Item> = (0..5).map(|i| Item(format!("turn {i}"))).collect();
        let r = context_collapse(
            &items,
            &cfg,
            &OkSampler,
            |early| early.iter().map(|i| i.0.as_str()).collect::<Vec<_>>().join("\n"),
            Item,
        )
        .await
        .unwrap();
        assert_eq!(r.collapsed_count, 3);
        assert_eq!(r.items.len(), 3); // carrier + 2 recent
        assert!(!r.summary.is_empty());
    }

    #[tokio::test]
    async fn no_collapse_when_under_keep_recent() {
        let cfg = ContextCollapseConfig::default();
        let items = vec![Item("a".into()), Item("b".into())];
        let r = context_collapse(
            &items,
            &cfg,
            &OkSampler,
            |early| early.iter().map(|i| i.0.as_str()).collect::<Vec<_>>().join("\n"),
            Item,
        )
        .await
        .unwrap();
        assert_eq!(r.collapsed_count, 0);
    }
}
