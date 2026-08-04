//! Agent 模块 - Agent 编排、循环工程、记忆、技能
//!
//! ## 4 层循环工程（对标 Claude Code）
//!
//! | 循环 | 模块 | 说明 |
//! |------|------|------|
//! | Turn | `loop_state` | LoopStateMachine 状态机驱动单轮对话 |
//! | Goal | `goal_loop` | /goal 目标驱动：自动拆子任务+验证+退出 |
//! | Schedule | `schedule_loop` | /schedule 定时循环：定时唤醒+执行+等待 |
//! | Proactive | `proactive_loop` | 闲置自主扫描代码质量+修复 |
//!
//! ## 其他核心能力
//!
//! - `doom_loop`: 3 级 Doom Loop 逃脱（注入提示→禁用工具→降级模型）
//! - `orchestrator`: 子 Agent 编排
//! - `subagent`: 子 Agent 创建与结果回传
//! - `skills`: 技能系统 + model 自动切换
//! - `prompt`: System prompt 构建
//! - `experiential`: ERL 经验反射学习（轨迹提取+启发式注入）

pub mod prompt;
pub mod subagent;
pub mod orchestrator;
pub mod doom_loop;
pub mod loop_state;
pub mod plan_execute;
pub mod skills;
pub mod goal_loop;
pub mod schedule_loop;
pub mod proactive_loop;

// 进阶模块（从 ccore-advanced re-export）
pub use ccore_advanced::agent::{experiential, meta_cognitive, decentralized, goal_verifier};

use serde::{Deserialize, Serialize};

/// Agent 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    /// 主 Agent：全功能编码
    Primary,
    /// 通用子 Agent
    GeneralPurpose,
    /// 只读代码探索
    Explore,
    /// 架构规划
    Plan,
    /// Codex 兼容工具集
    Codex,
}

impl std::str::FromStr for AgentType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "primary" => Self::Primary,
            "general-purpose" | "general" => Self::GeneralPurpose,
            "explore" => Self::Explore,
            "plan" => Self::Plan,
            "codex" => Self::Codex,
            _ => Self::GeneralPurpose,
        })
    }
}

/// Agent 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    /// 空闲，等待输入
    Idle,
    /// 正在思考（等待 LLM 响应）
    Thinking,
    /// 正在调用工具
    ToolCalling,
    /// 等待用户审批（Plan 模式）
    AwaitingApproval,
    /// 正在输出
    Outputting,
    /// 已完成
    Done,
    /// 出错
    Error,
}

/// Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent 类型
    pub agent_type: AgentType,
    /// 使用的模型
    pub model: String,
    /// 权限模式
    pub permission_mode: crate::node::PermissionMode,
    /// 最大轮次
    pub max_turns: Option<u32>,
    /// 是否启用子 Agent
    pub subagents_enabled: bool,
    /// 是否为非交互模式
    pub non_interactive: bool,
    /// 可用工具定义列表
    #[serde(default)]
    pub tools: Vec<crate::sampler::provider::ToolDefinition>,
}
