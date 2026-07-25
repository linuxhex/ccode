//! Tools 模块 - 工具桥接、Git Checkpoint、自动验证
//!
//! 工具系统架构：
//! - Agent 通过消息总线发送 ToolCallRequest 到 Tool Node
//! - Tool Node 通过 Bridge 层将请求转换为 grok Tool trait 的调用
//! - 工具执行结果（可能是流式）封装为 ToolCallResult 返回
//!
//! ccode 的工具调用在消息总线上是异步的：
//! - Agent 发 tool_call → Tool Node 执行 → 回传 tool_result
//! - 支持并行工具调用（多个 tool_call 可同时执行）
//! - 工具执行超时由 Tool Node 控制

pub mod bridge;
pub mod builtin;
pub mod checkpoint;
pub mod verify;

use serde::{Deserialize, Serialize};

/// 工具调用请求（消息总线传输格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    /// 工具调用 ID（LLM 返回的 tool_call_id）
    pub tool_call_id: String,
    /// 工具名称
    pub tool_name: String,
    /// 工具参数（JSON 格式，与 grok 工具的 Input 结构体对应）
    pub arguments: serde_json::Value,
    /// 发起方 Agent ID
    pub agent_id: String,
}

/// 工具调用结果（消息总线传输格式）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// 工具调用 ID（与请求对应）
    pub tool_call_id: String,
    /// 执行结果文本
    pub output: String,
    /// 是否成功
    pub success: bool,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// 是否为部分结果（流式中间结果）
    pub is_partial: bool,
}

/// 工具定义（用于注册到 LLM 的 tools 参数）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 参数 JSON Schema
    pub parameters: serde_json::Value,
}

/// 工具注册条目
#[derive(Debug, Clone)]
pub struct ToolEntry {
    /// 工具定义
    pub definition: ToolDefinition,
    /// 工具分类
    pub category: ToolCategory,
    /// 是否需要用户确认（受 PermissionMode 控制）
    pub requires_confirmation: bool,
    /// 是否为只读工具（不需要确认）
    pub read_only: bool,
}

/// 工具分类
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ToolCategory {
    /// 文件读写操作
    FileSystem,
    /// Shell 命令执行
    Shell,
    /// 搜索/检索
    Search,
    /// Web 访问
    Web,
    /// 任务管理
    Task,
    /// 用户交互
    UserInteraction,
    /// 代码分析
    CodeAnalysis,
    /// 图像/视频生成
    MediaGeneration,
    /// 记忆检索
    Memory,
}
