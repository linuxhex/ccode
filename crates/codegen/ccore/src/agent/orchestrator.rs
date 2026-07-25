//! Agent 编排器 - 管理主 Agent 和子 Agent 的交互

use std::collections::HashMap;
use crate::agent::subagent::{SubAgentState, SubAgentDefinition};
use crate::node::NodeId;

/// Agent 编排器
pub struct Orchestrator {
    /// 活跃的子 Agent
    subagents: HashMap<NodeId, SubAgentState>,
    /// 最大子 Agent 数量
    max_subagents: usize,
}

impl Orchestrator {
    pub fn new(max_subagents: usize) -> Self {
        Self {
            subagents: HashMap::new(),
            max_subagents,
        }
    }

    /// 是否可以创建新的子 Agent
    pub fn can_spawn(&self) -> bool {
        self.subagents.len() < self.max_subagents
    }

    /// 注册子 Agent
    pub fn register_subagent(&mut self, node_id: NodeId, definition: SubAgentDefinition) {
        let state = SubAgentState {
            node_id: node_id.clone(),
            definition,
            state: crate::agent::AgentState::Thinking,
            output: None,
        };
        self.subagents.insert(node_id, state);
    }

    /// 更新子 Agent 状态
    pub fn update_state(&mut self, node_id: &NodeId, state: crate::agent::AgentState) {
        if let Some(sub) = self.subagents.get_mut(node_id) {
            sub.state = state;
        }
    }

    /// 设置子 Agent 输出
    pub fn set_output(&mut self, node_id: &NodeId, output: String) {
        if let Some(sub) = self.subagents.get_mut(node_id) {
            sub.output = Some(output);
            sub.state = crate::agent::AgentState::Done;
        }
    }

    /// 移除已完成的子 Agent
    pub fn remove_completed(&mut self, node_id: &NodeId) {
        self.subagents.remove(node_id);
    }

    /// 获取所有活跃子 Agent
    pub fn active_subagents(&self) -> Vec<&SubAgentState> {
        self.subagents.values().collect()
    }

    /// 检查所有子 Agent 是否都已完成
    pub fn all_completed(&self) -> bool {
        self.subagents.values().all(|s| s.state == crate::agent::AgentState::Done)
    }
}
