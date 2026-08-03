//! Agent 工具调用状态类型
//!
//! 原 AgentNode 实现已被 ThinkerNode（仿生架构演进版）取代并移除，
//! 此模块仅保留 PendingToolCall 类型供 ThinkerNode 复用。

use serde::{Deserialize, Serialize};

/// Agent 工具调用状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}
