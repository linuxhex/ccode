//! Context Collapse — 大范围 LLM 摘要压缩。
//!
//! 当 MicroCompact + Snip + Budget 之后仍超 token 预算，对早期轮次做
//! LLM 摘要：保留关键决策、错误纠正、约束条件、已排除方案；丢弃闲聊、
//! 试错、重复。这是 5 层压缩管道的最后一层（Auto-Compact 触发后执行）。
//!
//! 对标 Claude Code 的 Context Collapse。LLM 调用通过 [`CollapseSampler`]
//! trait 注入，本模块不依赖具体 sampling crate。

use crate::item::{CompactionItem, CompactionRole};

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

/// 摘要 prompt 模板：指示 LLM 保留决策/纠正/约束、丢弃闲聊/试错。
pub const COLLAPSE_PROMPT: &str = "\
Summarize the following earlier conversation turns for context continuity.
PRESERVE: key decisions, error corrections, constraints, ruled-out approaches, project facts.
DROP: chitchat, trial-and-error, repeated content, raw tool output.
Be concise and factual. Output only the summary.";

#[cfg(test)]
mod tests {
    use super::*;

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
            |s| Item(s),
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
            |s| Item(s),
        )
        .await
        .unwrap();
        assert_eq!(r.collapsed_count, 0);
    }
}
