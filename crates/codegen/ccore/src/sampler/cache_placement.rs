//! Prompt cache breakpoint placement — 4-breakpoint strategy
//!
//! Inspired by Claude Code's `cache_edits.ts` and the fragment-driven cache
//! priority model. The Anthropic API supports up to 4 `cache_control`
//! breakpoints per request. We place them at:
//!
//! 1. **System prompt end** — Immutable prefix, rarely changes between turns.
//! 2. **Stable conversation prefix** — The older half of the conversation that
//!    stays the same across consecutive turns.
//! 3. **Recent context boundary** — Just before the last 2 turns, separating
//!    the rapidly-changing tail from the stable middle.
//! 4. **Penultimate message** — The second-to-last message, so only the final
//!    message falls outside the cache.
//!
//! Breakpoints 1 and 4 are always placed when possible. Breakpoints 2 and 3
//! are placed only when the conversation is long enough to benefit.

use super::provider::{CacheControl, ChatMessage};

/// Place cache breakpoints on a message list.
///
/// The caller must ensure messages are already grouped by role (consecutive
/// same-role messages will be merged by the provider). This function marks
/// up to 4 messages with `cache_control = Some(ephemeral)`.
pub fn place_breakpoints(messages: &mut [ChatMessage]) {
    if messages.is_empty() {
        return;
    }

    // Clear any existing breakpoints to avoid stale placements.
    for msg in messages.iter_mut() {
        msg.cache_control = None;
    }

    let len = messages.len();

    // BP1: system prompt end — the first message if it's a system role.
    if messages[0].role == "system" {
        messages[0].cache_control = Some(CacheControl::ephemeral());
    }

    if len < 3 {
        return;
    }

    // BP4: penultimate message — only the last message is uncached.
    messages[len - 2].cache_control = Some(CacheControl::ephemeral());

    if len < 7 {
        return;
    }

    // BP2: stable conversation prefix — at ~40% into the conversation
    // (after system message). This captures the older half that rarely
    // changes between consecutive turns.
    let conversation_start = if messages[0].role == "system" { 1 } else { 0 };
    let conversation_len = len - conversation_start;
    let stable_idx = conversation_start + conversation_len * 2 / 5;
    if stable_idx > 0 && stable_idx < len - 2 {
        messages[stable_idx].cache_control = Some(CacheControl::ephemeral());
    }

    // BP3: recent context boundary — 5 messages from the end (~2 turns).
    let recent_idx = len.saturating_sub(5);
    if recent_idx > stable_idx && recent_idx < len - 2 {
        messages[recent_idx].cache_control = Some(CacheControl::ephemeral());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            cache_control: None,
        }
    }

    fn breakpoint_indices(messages: &[ChatMessage]) -> Vec<usize> {
        messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.cache_control.is_some())
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn empty_messages() {
        let mut msgs: Vec<ChatMessage> = vec![];
        place_breakpoints(&mut msgs);
        assert!(breakpoint_indices(&msgs).is_empty());
    }

    #[test]
    fn single_system_message() {
        let mut msgs = vec![msg("system", "You are helpful.")];
        place_breakpoints(&mut msgs);
        assert_eq!(breakpoint_indices(&msgs), vec![0]);
    }

    #[test]
    fn short_conversation_two_breakpoints() {
        let mut msgs = vec![
            msg("system", "sys"),
            msg("user", "u1"),
            msg("assistant", "a1"),
            msg("user", "u2"),
            msg("assistant", "a2"),
        ];
        place_breakpoints(&mut msgs);
        let bp = breakpoint_indices(&msgs);
        // BP1 at 0 (system), BP4 at 3 (len-2=3)
        assert!(bp.contains(&0));
        assert!(bp.contains(&3));
        assert!(bp.len() <= 4);
    }

    #[test]
    fn long_conversation_four_breakpoints() {
        let mut msgs = vec![msg("system", "sys")];
        for i in 0..20 {
            msgs.push(msg("user", &format!("u{i}")));
            msgs.push(msg("assistant", &format!("a{i}")));
        }
        place_breakpoints(&mut msgs);
        let bp = breakpoint_indices(&msgs);
        // Should have exactly 4 breakpoints
        assert_eq!(bp.len(), 4);
        // BP1 is system (index 0)
        assert_eq!(bp[0], 0);
        // BP4 is penultimate (len-2 = 39)
        assert_eq!(bp[3], msgs.len() - 2);
    }

    #[test]
    fn no_system_message() {
        let mut msgs = vec![
            msg("user", "u1"),
            msg("assistant", "a1"),
            msg("user", "u2"),
            msg("assistant", "a2"),
            msg("user", "u3"),
            msg("assistant", "a3"),
            msg("user", "u4"),
            msg("assistant", "a4"),
        ];
        place_breakpoints(&mut msgs);
        let bp = breakpoint_indices(&msgs);
        // No system BP; should still place BP4 at len-2
        assert!(bp.contains(&(msgs.len() - 2)));
        assert!(bp.len() <= 4);
    }

    #[test]
    fn breakpoints_are_ordered() {
        let mut msgs = vec![msg("system", "sys")];
        for i in 0..15 {
            msgs.push(msg("user", &format!("u{i}")));
            msgs.push(msg("assistant", &format!("a{i}")));
        }
        place_breakpoints(&mut msgs);
        let bp = breakpoint_indices(&msgs);
        for window in bp.windows(2) {
            assert!(window[0] < window[1]);
        }
    }

    #[test]
    fn clears_stale_breakpoints() {
        let mut msgs = vec![
            msg("system", "sys"),
            msg("user", "u1"),
            msg("assistant", "a1"),
        ];
        msgs[1].cache_control = Some(CacheControl::ephemeral());
        msgs[2].cache_control = Some(CacheControl::ephemeral());
        place_breakpoints(&mut msgs);
        // Only system (0) and penultimate (1) should have breakpoints
        let bp = breakpoint_indices(&msgs);
        assert!(bp.contains(&0));
        assert_eq!(bp.len(), 2);
    }
}
