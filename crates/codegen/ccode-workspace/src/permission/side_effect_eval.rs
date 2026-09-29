//! Side-effect scope evaluation — 基于模型声明的副作用范围快速判定权限
//!
//! 借鉴 deepcode-cli 的副作用声明制。当 Bash 工具调用携带 `side_effects` 字段时，
//! 此模块在完整的 shell 解析之前提供一个快速路径：
//! - 纯只读 scope → 直接 Allow
//! - 危险 scope → 直接 Reject
//! - 中间 scope → Ask（交由现有权限流程）
//! - 未知 scope → Ask（安全阀）

use super::types::Decision;

/// 已知的副作用 scope 及其风险等级
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeRisk {
    /// 纯只读，始终允许
    ReadOnly,
    /// 项目内写入，需要确认
    WriteInProject,
    /// 项目外写入或 git 变更，需要确认
    WriteOutsideProject,
    /// 项目内删除，需要确认
    DeleteInProject,
    /// 项目外删除，直接拒绝
    DeleteOutsideProject,
    /// 网络访问，需要确认
    Network,
    /// 无法分类，安全阀
    Unknown,
}

fn classify_scope(scope: &str) -> ScopeRisk {
    match scope {
        "read-in-cwd" | "read-out-cwd" | "query-git-log" => ScopeRisk::ReadOnly,
        "write-in-cwd" => ScopeRisk::WriteInProject,
        "write-out-cwd" => ScopeRisk::WriteOutsideProject,
        "delete-in-cwd" => ScopeRisk::DeleteInProject,
        "delete-out-cwd" => ScopeRisk::DeleteOutsideProject,
        "mutate-git-log" => ScopeRisk::WriteOutsideProject,
        "network" => ScopeRisk::Network,
        _ => ScopeRisk::Unknown,
    }
}

/// 评估副作用声明，返回决策和原因标签。
///
/// 决策逻辑：
/// 1. 任何 scope 为 `DeleteOutsideProject` → Reject
/// 2. 任何 scope 为 `Unknown` → Ask（安全阀）
/// 3. 所有 scope 都是 `ReadOnly` → Allow
/// 4. 其余情况 → Ask（交由现有流程）
pub fn evaluate_side_effects(scopes: &[String]) -> (Decision, &'static str) {
    if scopes.is_empty() {
        return (Decision::Allow, "side_effect_empty_scopes");
    }

    let mut all_read_only = true;
    for scope in scopes {
        match classify_scope(scope) {
            ScopeRisk::DeleteOutsideProject => {
                return (
                    Decision::Reject(format!("side_effect_deny: {scope}")),
                    "side_effect_delete_outside_cwd",
                );
            }
            ScopeRisk::Unknown => {
                return (Decision::Ask, "side_effect_unknown_scope");
            }
            ScopeRisk::ReadOnly => {}
            _ => {
                all_read_only = false;
            }
        }
    }

    if all_read_only {
        (Decision::Allow, "side_effect_read_only")
    } else {
        (Decision::Ask, "side_effect_needs_approval")
    }
}

/// 判断 side_effects 是否提供了确定性的决策（Allow 或 Reject），
/// 还是需要一个 Ask 来走完整权限流程。
pub fn is_decisive(scopes: &[String]) -> bool {
    if scopes.is_empty() {
        return true;
    }
    for scope in scopes {
        match classify_scope(scope) {
            ScopeRisk::ReadOnly | ScopeRisk::DeleteOutsideProject => continue,
            ScopeRisk::Unknown => return false,
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scopes_allow() {
        let (decision, reason) = evaluate_side_effects(&[]);
        assert!(matches!(decision, Decision::Allow));
        assert_eq!(reason, "side_effect_empty_scopes");
    }

    #[test]
    fn read_only_scopes_allow() {
        let scopes = vec!["read-in-cwd".into(), "query-git-log".into()];
        let (decision, reason) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Allow));
        assert_eq!(reason, "side_effect_read_only");
    }

    #[test]
    fn write_in_cwd_asks() {
        let scopes = vec!["write-in-cwd".into()];
        let (decision, reason) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Ask));
        assert_eq!(reason, "side_effect_needs_approval");
    }

    #[test]
    fn delete_outside_cwd_rejects() {
        let scopes = vec!["delete-out-cwd".into()];
        let (decision, _) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Reject(_)));
    }

    #[test]
    fn unknown_scope_asks() {
        let scopes = vec!["something-weird".into()];
        let (decision, reason) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Ask));
        assert_eq!(reason, "side_effect_unknown_scope");
    }

    #[test]
    fn mixed_read_write_asks() {
        let scopes = vec!["read-in-cwd".into(), "write-in-cwd".into()];
        let (decision, _) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Ask));
    }

    #[test]
    fn delete_outside_overrides_all() {
        let scopes = vec![
            "read-in-cwd".into(),
            "write-in-cwd".into(),
            "delete-out-cwd".into(),
        ];
        let (decision, _) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Reject(_)));
    }

    #[test]
    fn network_asks() {
        let scopes = vec!["network".into()];
        let (decision, _) = evaluate_side_effects(&scopes);
        assert!(matches!(decision, Decision::Ask));
    }

    #[test]
    fn decisive_for_read_only() {
        assert!(is_decisive(&["read-in-cwd".into()]));
    }

    #[test]
    fn not_decisive_for_write() {
        assert!(!is_decisive(&["write-in-cwd".into()]));
    }

    #[test]
    fn not_decisive_for_unknown() {
        assert!(!is_decisive(&["bogus".into()]));
    }
}
