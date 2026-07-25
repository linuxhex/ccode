//! 子 Agent 定义与生命周期

use crate::agent::AgentType;
use crate::node::NodeId;
use serde::{Deserialize, Serialize};

/// 子 Agent 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentDefinition {
    /// 子 Agent 类型
    pub agent_type: AgentType,
    /// 使用的模型（如果不同于主 Agent）
    pub model: Option<String>,
    /// 子 Agent 的任务描述
    pub task_description: String,
    /// 最大轮次
    pub max_turns: u32,
}

/// 子 Agent 状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentState {
    /// 子 Agent 的 Node ID
    pub node_id: NodeId,
    /// 子 Agent 定义
    pub definition: SubAgentDefinition,
    /// 当前状态
    pub state: super::AgentState,
    /// 输出结果
    pub output: Option<String>,
}

/// 子 Agent spawn 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// 请求 ID
    pub request_id: String,
    /// 父 Agent ID
    pub parent_agent_id: String,
    /// 子 Agent 定义
    pub definition: SubAgentDefinition,
}

/// 子 Agent spawn 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResponse {
    /// 分配的 Node ID
    pub node_id: String,
}

/// 子 Agent 完成事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCompleted {
    pub node_id: String,
    pub output: String,
    pub success: bool,
}

/// 子 Agent 崩溃事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCrashed {
    pub node_id: String,
    pub error: String,
}
