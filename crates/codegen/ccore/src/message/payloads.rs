//! 总线 payload 类型（MessagePack 编解码，与现有 FrameCodec 一致）
//!
//! 各 Node 间通过消息总线传递的结构化数据。

use serde::{Deserialize, Serialize};

/// 工具权限请求（ToolNode → AcpNode/TUINode）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub agent_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub reason: Option<String>,
}

/// 工具权限回复（AcpNode/TUINode → ToolNode）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub agent_id: String,
    pub tool_call_id: String,
    pub allowed: bool,
    pub remember: bool,
}

/// 取消请求（AcpNode/TUINode → Thinker/Sampler/Tool）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    pub agent_id: String,
    pub reason: Option<String>,
}

/// 请求压缩会话上下文（Thinker → State）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactRequest {
    pub session_id: String,
    pub force: bool,
}

/// 压缩结果（State → Thinker）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactResult {
    pub session_id: String,
    pub ok: bool,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub error: Option<String>,
}
