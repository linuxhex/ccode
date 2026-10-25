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
    /// 流式累积的原始参数 JSON 片段（input_json_delta 增量拼接）。
    /// 参数分多个 chunk 到达，必须在分发时整体解析，逐 chunk 解析
    /// 会因部分 JSON 非法而失败（表现为 arguments=Null、bash 报"缺少 command 参数"）。
    #[serde(default)]
    pub arguments_raw: String,
}
