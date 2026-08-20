//! Prompt construction for conversation history compaction.
//!
//! The developer and user prompts are intentionally identical so the model
//! sees the instructions on both turns.

use anyhow::Result;

/// Builds the developer prompt to send to the compaction model.
pub fn format_compaction_developer_prompt() -> Result<String> {
    Ok(include_str!("../templates/compaction_developer_prompt.txt").to_string())
}

/// Builds the user prompt to send to the compaction model.
pub fn format_compaction_user_prompt() -> Result<String> {
    Ok(include_str!("../templates/compaction_user_prompt.txt").to_string())
}

/// Partial compact direction: which portion of the conversation to summarize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartialCompactDirection {
    /// Summarize the recent messages after retained context (from boundary).
    From,
    /// Summarize the prefix up to the boundary; summary precedes kept messages.
    UpTo,
}

/// Builds the partial compact prompt for the "from" direction.
///
/// The "from" direction summarizes only the recent messages that follow
/// earlier retained context, keeping the earlier context intact.
pub fn format_partial_compact_from_prompt() -> Result<String> {
    Ok(include_str!("../templates/compaction_partial_from_prompt.txt").to_string())
}

/// Builds the partial compact prompt for the "up_to" direction.
///
/// The "up_to" direction summarizes the prefix of the conversation;
/// the summary will be placed before newer kept messages, so it
/// emphasizes "Context for Continuing Work".
pub fn format_partial_compact_up_to_prompt() -> Result<String> {
    Ok(include_str!("../templates/compaction_partial_up_to_prompt.txt").to_string())
}

/// Selects the partial compact prompt by direction.
pub fn format_partial_compact_prompt(direction: PartialCompactDirection) -> Result<String> {
    match direction {
        PartialCompactDirection::From => format_partial_compact_from_prompt(),
        PartialCompactDirection::UpTo => format_partial_compact_up_to_prompt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_non_empty() {
        let dev = format_compaction_developer_prompt().expect("dev prompt renders");
        assert!(!dev.trim().is_empty(), "developer prompt empty");
        let user = format_compaction_user_prompt().expect("user prompt renders");
        assert!(!user.trim().is_empty(), "user prompt empty");
    }

    /// Belt-and-suspenders: the developer and user prompts are intentionally
    /// identical so the model sees the instructions on both turns. If you edit
    /// one, edit the other — this test catches drift.
    #[test]
    fn compaction_prompts_match() {
        let dev = format_compaction_developer_prompt().expect("dev prompt renders");
        let user = format_compaction_user_prompt().expect("user prompt renders");
        assert_eq!(
            dev, user,
            "compaction_developer_prompt.txt and compaction_user_prompt.txt must stay in sync"
        );
    }

    #[test]
    fn partial_compact_prompts_are_non_empty() {
        let from = format_partial_compact_from_prompt().expect("from prompt renders");
        assert!(!from.trim().is_empty(), "partial from prompt empty");
        let up_to = format_partial_compact_up_to_prompt().expect("up_to prompt renders");
        assert!(!up_to.trim().is_empty(), "partial up_to prompt empty");
    }

    #[test]
    fn partial_compact_prompts_differ() {
        let from = format_partial_compact_from_prompt().expect("from prompt renders");
        let up_to = format_partial_compact_up_to_prompt().expect("up_to prompt renders");
        assert_ne!(
            from, up_to,
            "partial from and up_to prompts should differ in scope instructions"
        );
    }

    #[test]
    fn partial_compact_direction_dispatch() {
        let from = format_partial_compact_prompt(PartialCompactDirection::From).expect("dispatch from");
        let up_to = format_partial_compact_prompt(PartialCompactDirection::UpTo).expect("dispatch up_to");
        assert_eq!(from, format_partial_compact_from_prompt().expect("from"));
        assert_eq!(up_to, format_partial_compact_up_to_prompt().expect("up_to"));
    }
}
