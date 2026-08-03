//! Compaction result validation (text-level, harness-agnostic).
//!
//! The Ccode chat's `validate_compaction_result(CcodeMessage, …)` wrapper in
//! the harness crate extracts the message text and delegates here.

use super::types::CompactionStrategy;

/// Errors from validating a compaction result before persisting.
#[derive(Debug)]
pub enum CompactionValidationError {
    /// The compaction output has no text content. Persisting an empty
    /// summary would be silently skipped on hydration while blocking
    /// future compaction triggers.
    EmptyContent,
    /// The compaction output is too short to be a meaningful summary,
    /// indicating the LLM produced a degenerate/hallucinated response.
    TooShort { actual: usize, min: usize },
    /// The compaction output contains excessive repetition, indicating
    /// the LLM looped on a fragment rather than producing a coherent summary.
    RepetitiveContent { repeated_ratio: f64 },
    /// DivideAndConquer `<chunk_summary>` XML tags are not balanced, indicating
    /// the LLM output was truncated or malformed. The content may be partially
    /// usable but signals an incomplete compaction.
    UnbalancedChunkTags { open: usize, close: usize },
}

impl std::fmt::Display for CompactionValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyContent => write!(f, "compaction message has empty text content"),
            Self::TooShort { actual, min } => {
                write!(
                    f,
                    "compaction summary too short: {} chars (minimum {})",
                    actual, min
                )
            }
            Self::RepetitiveContent { repeated_ratio } => {
                write!(
                    f,
                    "compaction summary has excessive repetition: {:.0}% duplicated text",
                    repeated_ratio * 100.0
                )
            }
            Self::UnbalancedChunkTags { open, close } => {
                write!(
                    f,
                    "unbalanced chunk_summary tags: {} open, {} close",
                    open, close
                )
            }
        }
    }
}

/// 最小摘要长度（字符数）：低于此值视为退化/幻觉摘要
const MIN_SUMMARY_CHARS: usize = 50;

/// 重复率阈值：超过此比例的重复文本视为 LLM 循环输出
const MAX_REPETITION_RATIO: f64 = 0.5;

/// Validate compaction output text before persisting.
///
/// Checks:
/// 1. Non-empty text content — an empty compaction would be silently skipped
///    on hydration while blocking future compaction triggers.
/// 2. Minimum length — a summary shorter than MIN_SUMMARY_CHARS is likely
///    degenerate or hallucinated.
/// 3. Repetition detection — if more than MAX_REPETITION_RATIO of the text
///    is repeated fragments, the LLM likely looped rather than summarizing.
/// 4. DivideAndConquer: balanced `<chunk_summary>` tags — unbalanced tags
///    indicate truncated LLM output.
pub fn validate_compaction_text(
    text_content: &str,
    strategy: &CompactionStrategy,
) -> Result<(), CompactionValidationError> {
    let trimmed = text_content.trim();

    // 1. Non-empty text content
    if trimmed.is_empty() {
        return Err(CompactionValidationError::EmptyContent);
    }

    // 2. Minimum length check
    if trimmed.len() < MIN_SUMMARY_CHARS {
        return Err(CompactionValidationError::TooShort {
            actual: trimmed.len(),
            min: MIN_SUMMARY_CHARS,
        });
    }

    // 3. Repetition detection: 检测文本中是否存在大段重复片段
    let repetition_ratio = detect_repetition_ratio(trimmed);
    if repetition_ratio > MAX_REPETITION_RATIO {
        return Err(CompactionValidationError::RepetitiveContent {
            repeated_ratio: repetition_ratio,
        });
    }

    // 4. DnC: validate chunk_summary tags are balanced
    if matches!(strategy, CompactionStrategy::DivideAndConquer) {
        let open_count = trimmed.matches("<chunk_summary").count();
        let close_count = trimmed.matches("</chunk_summary>").count();
        if open_count != close_count {
            return Err(CompactionValidationError::UnbalancedChunkTags {
                open: open_count,
                close: close_count,
            });
        }
    }

    Ok(())
}

/// 检测文本重复率：将文本按句号/换行分割为片段，统计重复片段占比
fn detect_repetition_ratio(text: &str) -> f64 {
    // 按句号、换行、分号分割为片段
    let fragments: Vec<&str> = text
        .split(|c| c == '.' || c == '\n' || c == ';' || c == '。')
        .map(|s| s.trim())
        .filter(|s| s.len() >= 10) // 只检测长度 >= 10 的有意义片段
        .collect();

    if fragments.is_empty() {
        return 0.0;
    }

    let mut seen = std::collections::HashMap::new();
    let mut repeated_count = 0usize;

    for fragment in &fragments {
        let count = seen.entry(fragment.to_lowercase()).or_insert(0usize);
        *count += 1;
        if *count >= 2 {
            // 第 2 次及以后出现均计为重复，准确反映重复占比
            repeated_count += 1;
        }
    }

    repeated_count as f64 / fragments.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_content_rejected() {
        assert!(matches!(
            validate_compaction_text("", &CompactionStrategy::Basic),
            Err(CompactionValidationError::EmptyContent)
        ));
        assert!(matches!(
            validate_compaction_text("  \n ", &CompactionStrategy::DivideAndConquer),
            Err(CompactionValidationError::EmptyContent)
        ));
    }

    #[test]
    fn valid_basic_accepted() {
        // 摘要需 >= 50 字符才被视为有效
        let summary = "This is a valid compaction summary with sufficient length to pass validation.";
        assert!(validate_compaction_text(summary, &CompactionStrategy::Basic).is_ok());
    }

    #[test]
    fn too_short_rejected() {
        assert!(matches!(
            validate_compaction_text("short", &CompactionStrategy::Basic),
            Err(CompactionValidationError::TooShort { .. })
        ));
    }

    #[test]
    fn repetitive_content_rejected() {
        let text = "This is a repeated fragment. This is a repeated fragment. This is a repeated fragment.";
        assert!(matches!(
            validate_compaction_text(text, &CompactionStrategy::Basic),
            Err(CompactionValidationError::RepetitiveContent { .. })
        ));
    }

    #[test]
    fn unbalanced_dnc_tags_rejected() {
        let text = "<chunk_summary index=\"0\">\nThis is a summary with enough length to pass minimum checks.\n</chunk_summary>\n<chunk_summary index=\"1\">\nmissing close tag but has enough text";
        assert!(matches!(
            validate_compaction_text(text, &CompactionStrategy::DivideAndConquer),
            Err(CompactionValidationError::UnbalancedChunkTags { open: 2, close: 1 })
        ));
    }

    #[test]
    fn balanced_dnc_tags_accepted() {
        let text = "<chunk_summary index=\"0\">\nsummary zero with sufficient length\n</chunk_summary>\n<chunk_summary index=\"1\">\nsummary one with sufficient length\n</chunk_summary>";
        assert!(validate_compaction_text(text, &CompactionStrategy::DivideAndConquer).is_ok());
    }

    #[test]
    fn basic_ignores_unbalanced_tags() {
        let text = "<chunk_summary index=\"0\">no close tag but the text is long enough to pass validation";
        assert!(validate_compaction_text(text, &CompactionStrategy::Basic).is_ok());
    }
}
