//! Provider trait - LLM 后端统一接口

use anyhow::Result;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

/// 采样请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleRequest {
    /// 请求唯一 ID
    pub request_id: String,
    /// 发起请求的 Agent ID
    pub agent_id: String,
    /// 模型名称
    pub model: String,
    /// 对话消息列表
    pub messages: Vec<ChatMessage>,
    /// 可用工具定义
    pub tools: Vec<ToolDefinition>,
    /// 是否流式返回
    pub stream: bool,
    /// 推理强度 (0.0 - 1.0)
    pub reasoning_effort: Option<f64>,
    /// 最大生成 token 数
    pub max_tokens: Option<u32>,
    /// 温度
    pub temperature: Option<f64>,
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// 工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 流式返回块
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// 请求 ID
    pub request_id: String,
    /// 通道：text / reasoning / tool_call
    pub channel: StreamChannel,
    /// 内容
    pub content: String,
    /// 工具调用信息（仅 tool_call 通道）
    pub tool_call: Option<ToolCallChunk>,
}

/// 流式通道
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum StreamChannel {
    Text,
    Reasoning,
    ToolCall,
}

/// 工具调用块
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallChunk {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: String,
}

/// 采样完成响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleResponse {
    pub request_id: String,
    pub usage: TokenUsage,
    pub finish_reason: String,
}

/// Token 使用量
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Provider trait - 所有 LLM 后端必须实现
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// 流式采样
    async fn stream(
        &self,
        request: SampleRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>>;

    /// 非流式采样
    async fn sample(&self, request: SampleRequest) -> Result<SampleResponse>;

    /// 返回此 Provider 支持的模型列表
    fn model_list(&self) -> &[String];

    /// Provider 名称
    fn name(&self) -> &str;
}
