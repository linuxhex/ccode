//! COMPACTABLE_TOOLS 白名单 — 微压缩/预算/截断共享的工具分类。
//!
//! 从 `micro_compact.rs` 抽出，供 [`crate::budget`]、[`crate::snip`]、
//! [`crate::micro_compact`] 共享同一份白名单，避免三处各自维护导致漂移。
//!
//! 设计原则：白名单只覆盖"输出大、语义可由调用意图推断"的工具。
//! 编排类工具（AgentTool / Task / TodoWrite / AskUserQuestion 等）的
//! 结果携带不可重建的语义，永远不进压缩白名单。

/// 可压缩的工具白名单。
///
/// 只有这些工具的输出结果才会在微压缩/预算截断中被清除或摘要化，
/// 其他工具结果（如 AgentTool 编排结果）保持不动。
///
/// 大小写不敏感匹配（见 [`is_compactable`]），确保 `FileRead` / `fileread` /
/// `ReadFile` 等不同命名风格都能命中。
pub const COMPACTABLE_TOOLS: &[&str] = &[
    "FileRead", "Bash", "Grep", "Glob", "WebSearch", "WebFetch", "FileEdit", "FileWrite",
    "ListDir", "ReadFile",
];

/// 判断工具是否在可压缩白名单中。
///
/// 大小写不敏感，确保不同命名风格都能命中。
pub fn is_compactable(tool_name: &str) -> bool {
    COMPACTABLE_TOOLS
        .iter()
        .any(|t| t.eq_ignore_ascii_case(tool_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_covers_io_tools() {
        assert!(is_compactable("FileRead"));
        assert!(is_compactable("Bash"));
        assert!(is_compactable("Grep"));
        assert!(is_compactable("FileWrite"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_compactable("fileread"));
        assert!(is_compactable("BASH"));
        assert!(is_compactable("ReadFile"));
    }

    #[test]
    fn orchestration_tools_are_not_compactable() {
        assert!(!is_compactable("AgentTool"));
        assert!(!is_compactable("Task"));
        assert!(!is_compactable("TodoWrite"));
        assert!(!is_compactable("AskUserQuestion"));
        assert!(!is_compactable("UnknownTool"));
    }
}
