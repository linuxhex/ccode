//! 子代理工具白名单
//!
//! 根据子代理类型（Primary / AsyncAgent / Teammate / Coordinator）
//! 过滤可用工具列表，确保子代理只能访问其权限级别允许的工具。
//!
//! 设计原则：
//! - 主代理拥有所有工具权限
//! - 异步子代理只能使用读写工具，禁止交互式和编排工具
//! - 团队成员在异步子代理基础上增加任务管理和消息工具
//! - 协调者只能使用编排工具（AgentTool、TaskStop、SendMessage 等）
//! - 所有子代理禁止递归创建子代理（AgentTool）和交互式提问

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// 工具权限级别
///
/// 将工具按功能分类为三个权限级别，
// 用于控制不同类型子代理的工具访问范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolLevel {
    /// 读写工具：FileRead、Grep、Glob、WebSearch、WebFetch、FileEdit、FileWrite、Bash
    ReadWrite,
    /// 交互式工具：AskUserQuestion、EnterPlanMode、ExitPlanMode
    Interactive,
    /// 编排工具：AgentTool、TaskStop、SendMessage
    Orchestrator,
}

/// 子代理类型
///
/// 不同类型的子代理拥有不同的工具权限：
/// - Primary：主代理，所有工具可用
/// - AsyncAgent：异步子代理，只允许 ReadWrite 级别工具
/// - Teammate：团队成员，ReadWrite + 任务管理 + 消息
/// - Coordinator：协调者，只允许 Orchestrator 级别工具
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentType {
    /// 主代理（所有工具可用）
    Primary,
    /// 异步子代理（只允许 ReadWrite 级别工具）
    AsyncAgent,
    /// 团队成员（ReadWrite + 任务管理 + 消息）
    Teammate,
    /// 协调者（只允许 Orchestrator 级别工具）
    Coordinator,
}

/// 所有子代理禁止使用的工具
///
/// 这些工具涉及全局状态或交互式操作，子代理不应直接使用：
/// - AgentTool：禁止递归创建子代理
/// - ExitPlanMode / EnterPlanMode：Plan 模式是主线程抽象
/// - AskUserQuestion：子代理不能交互式提问
/// - TaskStop：需要主线程任务状态
pub const AGENT_DISALLOWED_TOOLS: &[&str] = &[
    "AgentTool",       // 禁止递归创建子代理
    "ExitPlanMode",    // Plan 模式是主线程抽象
    "EnterPlanMode",   // Plan 模式是主线程抽象
    "AskUserQuestion", // 子代理不能交互式提问
    "TaskStop",        // 需要主线程任务状态
];

/// 异步子代理允许的工具
///
/// 包含所有读写操作和基础工具，但不包括交互式和编排工具。
pub const ASYNC_AGENT_ALLOWED_TOOLS: &[&str] = &[
    "FileRead", "WebSearch", "TodoWrite", "Grep",
    "WebFetch", "Glob", "Bash", "FileEdit", "FileWrite",
    "Skill", "NotebookEdit", "ToolSearch",
];

/// 团队成员额外允许的工具
///
/// 在异步子代理基础上，增加任务管理和消息通信工具。
pub const TEAMMATE_EXTRA_TOOLS: &[&str] = &[
    "TaskCreate", "TaskGet", "TaskList", "TaskUpdate",
    "SendMessage", "CronCreate", "CronDelete", "CronList",
];

/// 协调者允许的工具
///
/// 协调者专注于编排和通信，不直接操作文件系统。
pub const COORDINATOR_ALLOWED_TOOLS: &[&str] = &[
    "AgentTool", "TaskStop", "SendMessage", "SyntheticOutput",
];

/// 根据代理类型过滤可用工具
///
/// 从全部工具列表中，按代理类型和权限规则过滤出可用工具：
/// - Primary：返回全部工具
/// - AsyncAgent：仅保留白名单中且不在禁止列表中的工具
/// - Teammate：AsyncAgent 白名单 + 团队成员额外工具，排除禁止列表
/// - Coordinator：仅保留协调者白名单中的工具
pub fn filter_tools_for_agent(
    agent_type: AgentType,
    all_tools: &[String],
) -> Vec<String> {
    let disallowed: HashSet<&str> = AGENT_DISALLOWED_TOOLS.iter().copied().collect();

    match agent_type {
        AgentType::Primary => all_tools.to_vec(),
        AgentType::AsyncAgent => {
            let allowed: HashSet<&str> = ASYNC_AGENT_ALLOWED_TOOLS.iter().copied().collect();
            all_tools
                .iter()
                .filter(|t| {
                    let name = t.as_str();
                    !disallowed.contains(name) && allowed.contains(name)
                })
                .cloned()
                .collect()
        }
        AgentType::Teammate => {
            let allowed: HashSet<&str> = ASYNC_AGENT_ALLOWED_TOOLS
                .iter()
                .chain(TEAMMATE_EXTRA_TOOLS.iter())
                .copied()
                .collect();
            all_tools
                .iter()
                .filter(|t| {
                    let name = t.as_str();
                    !disallowed.contains(name) && allowed.contains(name)
                })
                .cloned()
                .collect()
        }
        AgentType::Coordinator => {
            let allowed: HashSet<&str> = COORDINATOR_ALLOWED_TOOLS.iter().copied().collect();
            all_tools
                .iter()
                .filter(|t| allowed.contains(t.as_str()))
                .cloned()
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_tools() -> Vec<String> {
        vec![
            "FileRead".into(), "Bash".into(), "Grep".into(), "Glob".into(),
            "WebSearch".into(), "WebFetch".into(), "FileEdit".into(), "FileWrite".into(),
            "TodoWrite".into(), "AgentTool".into(), "AskUserQuestion".into(),
            "EnterPlanMode".into(), "ExitPlanMode".into(), "TaskStop".into(),
            "TaskCreate".into(), "SendMessage".into(), "SyntheticOutput".into(),
        ]
    }

    #[test]
    fn test_primary_has_all_tools() {
        let tools = filter_tools_for_agent(AgentType::Primary, &all_tools());
        assert_eq!(tools.len(), all_tools().len());
    }

    #[test]
    fn test_async_agent_excludes_disallowed() {
        let tools = filter_tools_for_agent(AgentType::AsyncAgent, &all_tools());
        assert!(!tools.iter().any(|t| t == "AgentTool"));
        assert!(!tools.iter().any(|t| t == "AskUserQuestion"));
        assert!(tools.iter().any(|t| t == "FileRead"));
        assert!(tools.iter().any(|t| t == "Bash"));
    }

    #[test]
    fn test_teammate_has_extra_tools() {
        let tools = filter_tools_for_agent(AgentType::Teammate, &all_tools());
        assert!(tools.iter().any(|t| t == "TaskCreate"));
        assert!(tools.iter().any(|t| t == "SendMessage"));
        assert!(!tools.iter().any(|t| t == "AgentTool")); // 禁止列表
    }

    #[test]
    fn test_coordinator_only_orchestrator() {
        let tools = filter_tools_for_agent(AgentType::Coordinator, &all_tools());
        assert!(tools.iter().any(|t| t == "AgentTool"));
        assert!(tools.iter().any(|t| t == "SendMessage"));
        assert!(!tools.iter().any(|t| t == "FileRead"));
        assert!(!tools.iter().any(|t| t == "Bash"));
    }
}
