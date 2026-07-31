//! Budget Reduction — 工具输出 token 预算。
//!
//! 每个可压缩工具有独立的输出 token 上限。超预算的输出截断并标注
//! `[truncated, N tokens saved]`，保留首尾以便模型仍能看到调用是否成功
//! 和尾部最近的结果。
//!
//! 对标 Claude Code 的 Budget Reduction 层（5 层压缩管道的第 1 层）。
//! 与 [`crate::compactable`] 共享白名单：不在白名单内的工具不做预算检查，
//! 原样返回。

use std::collections::HashMap;

use crate::compactable::is_compactable;

/// 默认每工具输出 token 预算。
///
/// 值参考 Claude Code：FileRead/Grep 偏小（结构化、信息密度高），
/// Bash 偏大（命令输出冗长但常含关键尾部错误）。
pub const DEFAULT_BUDGETS: &[(&str, usize)] = &[
    ("FileRead", 4000),
    ("ReadFile", 4000),
    ("Bash", 8000),
    ("Grep", 4000),
    ("Glob", 2000),
    ("ListDir", 2000),
    ("WebSearch", 2000),
    ("WebFetch", 4000),
    ("FileEdit", 4000),
    ("FileWrite", 4000),
];

/// 截断标注模板。`{n}` = 估算节省的 token 数。
pub const TRUNCATED_NOTE: &str = "[truncated, {n} tokens saved]";

/// 工具名 → 输出 token 上限。
#[derive(Debug, Clone)]
pub struct ToolBudget {
    limits: HashMap<String, usize>,
}

impl Default for ToolBudget {
    fn default() -> Self {
        Self::from_defaults()
    }
}

impl ToolBudget {
    /// 从 [`DEFAULT_BUDGETS`] 构造。
    pub fn from_defaults() -> Self {
        let mut limits = HashMap::with_capacity(DEFAULT_BUDGETS.len());
        for (name, cap) in DEFAULT_BUDGETS {
            limits.insert((*name).to_string(), *cap);
        }
        Self { limits }
    }

    /// 覆盖或新增某工具的预算。
    pub fn set(&mut self, tool: &str, cap: usize) -> &mut Self {
        self.limits.insert(tool.to_string(), cap);
        self
    }

    /// 某工具的预算上限；不在表中返回 `None`。
    pub fn limit(&self, tool: &str) -> Option<usize> {
        self.limits.get(tool).copied()
    }

    /// 预算检查结果。
    pub fn check(&self, tool: &str, output_tokens: usize) -> BudgetResult {
        // 非可压缩工具一律放行（不在白名单内不做预算）。
        if !is_compactable(tool) {
            return BudgetResult::NotApplicable;
        }
        match self.limit(tool) {
            Some(cap) if output_tokens > cap => BudgetResult::Over {
                cap,
                overage: output_tokens - cap,
            },
            _ => BudgetResult::Within,
        }
    }

    /// 按预算截断输出。
    ///
    /// 超预算时保留首尾各 `cap / 2` token（按 `chars / 4` 估算 token），
    /// 中间插入截断标注。未超预算或非可压缩工具原样返回。
    pub fn truncate(&self, tool: &str, output: &str) -> BudgetTruncateResult {
        let est_tokens = estimate_tokens(output);
        match self.check(tool, est_tokens) {
            BudgetResult::Within | BudgetResult::NotApplicable => {
                BudgetTruncateResult::unchanged(output)
            }
            BudgetResult::Over { cap, .. } => {
                let keep_tokens = cap / 2;
                let keep_chars = keep_tokens * 4;
                if output.len() <= keep_chars * 2 {
                    // 输出比要保留的还短，不截断。
                    return BudgetTruncateResult::unchanged(output);
                }
                let head = &output[..keep_chars.min(output.len())];
                let tail_start = output.len().saturating_sub(keep_chars);
                let tail = &output[tail_start.max(keep_chars)..];
                let saved = est_tokens.saturating_sub(cap);
                let note = TRUNCATED_NOTE.replace("{n}", &saved.to_string());
                let truncated = format!("{head}\n{note}\n{tail}");
                BudgetTruncateResult {
                    output: truncated,
                    original_tokens: est_tokens,
                    saved_tokens: saved,
                    truncated: true,
                }
            }
        }
    }
}

/// 预算检查结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetResult {
    /// 在预算内。
    Within,
    /// 超预算。
    Over { cap: usize, overage: usize },
    /// 非可压缩工具，不做预算检查。
    NotApplicable,
}

/// 截断结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetTruncateResult {
    /// 截断后（或原样）的输出。
    pub output: String,
    /// 原输出估算 token 数。
    pub original_tokens: usize,
    /// 节省的 token 数（未截断为 0）。
    pub saved_tokens: usize,
    /// 是否发生了截断。
    pub truncated: bool,
}

impl BudgetTruncateResult {
    fn unchanged(output: &str) -> Self {
        Self {
            output: output.to_string(),
            original_tokens: estimate_tokens(output),
            saved_tokens: 0,
            truncated: false,
        }
    }
}

/// 粗略 token 估算：`chars / 4`。
///
/// 压缩管线只用于预算决策，不需要精确 tokenizer；与 ccode-build 的
/// `bytes / 4` 估算一致。
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_budget_is_unchanged() {
        let b = ToolBudget::from_defaults();
        let out = "x".repeat(100); // ~25 tokens
        let r = b.truncate("FileRead", &out);
        assert!(!r.truncated);
        assert_eq!(r.saved_tokens, 0);
    }

    #[test]
    fn over_budget_truncates_with_note() {
        let b = ToolBudget::from_defaults();
        // FileRead cap=4000 tokens → 16000 chars. 制造 20000 chars。
        let out = "a".repeat(20_000);
        let r = b.truncate("FileRead", &out);
        assert!(r.truncated);
        assert!(r.output.contains("tokens saved"));
        assert!(r.saved_tokens > 0);
        // 首尾都被保留
        assert!(r.output.starts_with('a'));
        assert!(r.output.ends_with('a'));
    }

    #[test]
    fn non_compactable_tool_is_not_applicable() {
        let b = ToolBudget::from_defaults();
        let out = "x".repeat(100_000);
        let r = b.truncate("AgentTool", &out);
        assert!(!r.truncated);
        assert_eq!(r.saved_tokens, 0);
    }

    #[test]
    fn override_limit() {
        let mut b = ToolBudget::from_defaults();
        b.set("Bash", 100);
        assert_eq!(b.limit("Bash"), Some(100));
        let out = "y".repeat(10_000);
        let r = b.truncate("Bash", &out);
        assert!(r.truncated);
    }
}
