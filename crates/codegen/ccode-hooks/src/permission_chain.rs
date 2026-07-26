//! # 权限决策链
//!
//! 借鉴 Claude Code 的 hasPermissionsToUseTool 完整链路设计：
//! 权限决策不是简单的 allow/deny，而是一个多阶段决策链：
//! - 预过滤（已知危险命令直接拒绝）
//! - Hook 拦截（用户自定义拦截）
//! - 规则引擎（规则匹配）
//! - 用户确认（手动审批）
//!
//! deny 后 Agent 可以知道被拒了，并调整策略（deny recovery）；
//! 最终决策附带 available_alternatives（可替代方案）。

use crate::permission_rules::{PermissionDecision, PermissionRuleSet};
use crate::result::HookDecision;
use serde::{Deserialize, Serialize};

/// 权限决策链的完整结果
///
/// 包含最终决策、决策来源、可替代方案和是否可重试等信息，
/// 让 Agent 在被拒绝后能够理解原因并调整策略。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionChainResult {
    /// 最终决策
    pub decision: PermissionDecision,
    /// 决策来源
    pub source: DecisionSource,
    /// 可替代方案（deny 时提供建议）
    pub alternatives: Vec<String>,
    /// 是否可以重试（换个参数可能通过）
    pub retryable: bool,
}

/// 决策来源
///
/// 标识权限决策在链路中的哪个阶段产生，
/// 便于调试和日志追踪。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DecisionSource {
    /// 预过滤（已知危险命令，如 rm -rf /）
    PreFilter,
    /// Hook 拦截
    Hook { hook_name: String },
    /// 规则引擎匹配
    RuleEngine,
    /// 用户手动确认
    UserConfirmation,
    /// 默认策略
    Default,
}

/// 预过滤规则：已知危险命令直接拒绝
///
/// 在规则引擎之前执行，对已知的高危操作进行拦截：
/// - 危险 shell 命令（rm -rf /、mkfs、dd if=/dev/zero 等）
/// - 敏感文件写入（.env、credentials、.ssh/、id_rsa 等）
///
/// 预过滤拦截的请求不可重试，因为操作本身就是危险的。
pub fn pre_filter(tool_name: &str, input: &serde_json::Value) -> Option<PermissionChainResult> {
    // 危险命令黑名单
    let dangerous_commands = [
        "rm -rf /",
        "rm -rf /*",
        "mkfs",
        "dd if=/dev/zero",
        ":(){ :|:& };:",
        "chmod -R 777 /",
        "curl | sh",
        "wget | sh",
    ];

    // 检查危险 shell 命令
    if tool_name == "Bash" || tool_name == "Shell" {
        if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
            let cmd_lower = cmd.to_lowercase();
            for dangerous in &dangerous_commands {
                if cmd_lower.contains(dangerous) {
                    return Some(PermissionChainResult {
                        decision: PermissionDecision::Deny {
                            reason: format!("预过滤拒绝：命令包含危险操作 '{}'", dangerous),
                        },
                        source: DecisionSource::PreFilter,
                        alternatives: vec![
                            "如果需要删除文件，请指定具体路径".to_string(),
                            "使用 git clean 替代 rm -rf".to_string(),
                        ],
                        retryable: false,
                    });
                }
            }
        }
    }

    // 检查敏感文件写入
    if tool_name == "FileWrite"
        || tool_name == "Write"
        || tool_name == "FileEdit"
        || tool_name == "Edit"
    {
        if let Some(path) = input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str())
        {
            let sensitive_files = [".env", "credentials", ".ssh/", "id_rsa", ".gnupg/"];
            for sensitive in &sensitive_files {
                if path.contains(sensitive) {
                    return Some(PermissionChainResult {
                        decision: PermissionDecision::Deny {
                            reason: format!("预过滤拒绝：写入敏感文件 '{}'", sensitive),
                        },
                        source: DecisionSource::PreFilter,
                        alternatives: vec![
                            "使用环境变量替代硬编码密钥".to_string(),
                            "使用 secrets 管理工具".to_string(),
                        ],
                        retryable: false,
                    });
                }
            }
        }
    }

    None
}

/// 执行完整权限决策链
///
/// 按优先级依次检查：
/// 1. 预过滤 — 已知危险操作直接拒绝
/// 2. Hook 拦截 — 用户自定义 hook 的 deny 决策
/// 3. 规则引擎 — 规则集匹配
/// 4. 默认策略 — 无匹配规则时，auto 模式放行，否则需确认
pub fn evaluate_permission_chain(
    tool_name: &str,
    tool_input: &serde_json::Value,
    rules: &PermissionRuleSet,
    hook_decision: Option<HookDecision>,
    auto_mode: bool,
) -> PermissionChainResult {
    // 1. 预过滤：已知危险命令直接拒绝
    if let Some(result) = pre_filter(tool_name, tool_input) {
        return result;
    }

    // 2. Hook 拦截：用户自定义 hook 的 deny 决策
    if let Some(HookDecision::Deny { reason, .. }) = hook_decision {
        return PermissionChainResult {
            decision: PermissionDecision::Deny { reason },
            source: DecisionSource::Hook {
                hook_name: String::new(),
            },
            alternatives: Vec::new(),
            retryable: false,
        };
    }

    // 3. 规则引擎：按规则集匹配
    let rule_decision = rules.evaluate(tool_name, tool_input);
    match rule_decision {
        PermissionDecision::Deny { reason } => {
            return PermissionChainResult {
                decision: PermissionDecision::Deny { reason },
                source: DecisionSource::RuleEngine,
                alternatives: Vec::new(),
                retryable: true,
            };
        }
        PermissionDecision::Ask { reason } if !auto_mode => {
            // 非 auto 模式下，ask 需要用户确认
            return PermissionChainResult {
                decision: PermissionDecision::Ask { reason },
                source: DecisionSource::RuleEngine,
                alternatives: Vec::new(),
                retryable: true,
            };
        }
        PermissionDecision::Allow => {
            return PermissionChainResult {
                decision: PermissionDecision::Allow,
                source: DecisionSource::RuleEngine,
                alternatives: Vec::new(),
                retryable: false,
            };
        }
        _ => {} // auto 模式下的 Ask 走默认策略
    }

    // 4. 默认策略：无匹配规则时
    if auto_mode {
        PermissionChainResult {
            decision: PermissionDecision::Allow,
            source: DecisionSource::Default,
            alternatives: Vec::new(),
            retryable: false,
        }
    } else {
        PermissionChainResult {
            decision: PermissionDecision::Ask {
                reason: format!("未找到 {} 的权限规则，需要确认", tool_name),
            },
            source: DecisionSource::Default,
            alternatives: Vec::new(),
            retryable: true,
        }
    }
}
