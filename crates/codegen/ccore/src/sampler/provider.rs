//! Provider trait - LLM 后端统一接口

use anyhow::Result;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    /// 推理强度 (0.0 - 1.0)，对应 DeepSeek R1/GLM/Qwen 的 reasoning_effort
    pub reasoning_effort: Option<f64>,
    /// 最大生成 token 数
    pub max_tokens: Option<u32>,
    /// 温度
    pub temperature: Option<f64>,
    /// top_p 核采样参数
    pub top_p: Option<f64>,
    /// 思维链/推理模式配置（DeepSeek R1 / GLM-5 / Qwen thinking）
    pub thinking: Option<ThinkingConfig>,
    /// 系统提示（独立于 messages）
    pub system_prompt: Option<String>,
    /// 工具选择策略
    pub tool_choice: Option<ToolChoice>,
    /// Prompt cache key（复用 API 侧 KV cache）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// 是否为 GoalLoop 子任务验证请求（SamplerNode 识别后走验证闭环，
    /// 流式收集完整响应并解析 JSON，结果发到 cortex/goal_verify_result）
    #[serde(default)]
    pub goal_verify: bool,
    /// 额外的 HTTP 请求头（如 x-api-key、anthropic-version 等）
    #[serde(default)]
    pub extra_headers: std::collections::HashMap<String, String>,
}

/// 思维链/推理模式配置
///
/// 不同模型对 thinking 参数的支持方式不同：
/// - DeepSeek R1: `thinking: {type: "enabled"}` 或 `reasoning_effort`
/// - GLM-5.0: `thinking: {type: "enabled"}`
/// - Qwen: `enable_thinking: true`
/// - Claude: `thinking: {type: "enabled", budget_tokens: 4096}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// 是否启用思维链（对应 OpenAI 兼容格式的 `thinking.type`）
    pub enabled: bool,
    /// 思维链 token 预算（仅 Claude 扩展思考模式使用）
    #[serde(default)]
    pub budget_tokens: Option<u32>,
}

impl ThinkingConfig {
    /// 创建启用思维链的配置
    pub fn enabled() -> Self {
        Self { enabled: true, budget_tokens: None }
    }

    /// 创建带 token 预算的思维链配置
    pub fn with_budget(budget_tokens: u32) -> Self {
        Self { enabled: true, budget_tokens: Some(budget_tokens) }
    }
}

/// Prompt cache 控制标记
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheControl {
    /// 缓存类型（当前仅支持 ephemeral）
    #[serde(rename = "type")]
    pub cache_type: String,
}

impl CacheControl {
    pub fn ephemeral() -> Self {
        Self { cache_type: "ephemeral".to_string() }
    }
}

/// 聊天消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// API 侧 prompt cache 断点标记（类似 Claude Code cache_edits）
    /// 插入在 system prompt 末尾和最近 N 轮对话之前，
    /// 让 API 复用已计算的前缀 KV cache
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
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
    /// Token 使用量（最后一个 chunk 或包含 usage 的 chunk 中携带）
    pub usage: Option<TokenUsage>,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Provider trait - 所有 LLM 后端必须实现
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// 流式采样（支持取消信号）
    async fn stream(
        &self,
        request: SampleRequest,
        cancel: CancellationHandle,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>>;

    /// 非流式采样
    async fn sample(&self, request: SampleRequest) -> Result<SampleResponse>;

    /// 返回此 Provider 支持的模型列表
    fn model_list(&self) -> &[String];

    /// Provider 名称
    fn name(&self) -> &str;
}

/// 工具选择策略
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolChoice {
    /// 自动决定
    Auto,
    /// 必须调用工具
    Required,
    /// 不调用工具
    None,
    /// 指定工具
    Specific { name: String },
}

/// 采样器事件（供外部消费）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SamplerEvent {
    /// 采样开始
    Started { request_id: String, model: String },
    /// 文本增量
    TextDelta { request_id: String, delta: String },
    /// 思考/推理增量
    ThinkingDelta { request_id: String, delta: String },
    /// 工具调用开始
    ToolCallStart { request_id: String, tool_call_id: String, tool_name: String },
    /// 工具调用参数增量
    ToolCallDelta { request_id: String, tool_call_id: String, delta: String },
    /// 工具调用结束
    ToolCallEnd { request_id: String, tool_call_id: String },
    /// 采样完成
    Completed { request_id: String, usage: TokenUsage, finish_reason: String },
    /// 采样错误
    Error { request_id: String, error: String },
    /// 采样取消
    Cancelled { request_id: String },
}

/// 取消句柄
#[derive(Debug, Clone)]
pub struct CancellationHandle {
    cancelled: Arc<AtomicBool>,
}

impl CancellationHandle {
    pub fn new() -> Self {
        Self { cancelled: Arc::new(AtomicBool::new(false)) }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancellationHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// 内容块类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    /// 文本内容
    Text { text: String },
    /// 工具调用
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    /// 思考内容（Claude extended thinking / DeepSeek reasoning）
    Thinking { thinking: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sampler_event_serialization() {
        let event = SamplerEvent::Started {
            request_id: "req-1".into(),
            model: "gpt-4".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Started"));
        assert!(json.contains("req-1"));

        let event = SamplerEvent::TextDelta {
            request_id: "req-1".into(),
            delta: "Hello".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("TextDelta"));

        let event = SamplerEvent::ThinkingDelta {
            request_id: "req-1".into(),
            delta: "Let me think".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ThinkingDelta"));

        let event = SamplerEvent::ToolCallStart {
            request_id: "req-1".into(),
            tool_call_id: "call-1".into(),
            tool_name: "bash".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ToolCallStart"));

        let event = SamplerEvent::Completed {
            request_id: "req-1".into(),
            usage: TokenUsage { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 },
            finish_reason: "stop".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Completed"));

        let event = SamplerEvent::Cancelled { request_id: "req-1".into() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("Cancelled"));

        // 往返测试
        let event = SamplerEvent::Error {
            request_id: "req-1".into(),
            error: "timeout".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SamplerEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, SamplerEvent::Error { .. }));
    }

    #[test]
    fn test_cancellation_handle() {
        let handle = CancellationHandle::new();
        assert!(!handle.is_cancelled());

        handle.cancel();
        assert!(handle.is_cancelled());
    }

    #[test]
    fn test_cancellation_handle_clone() {
        let handle = CancellationHandle::new();
        let cloned = handle.clone();

        assert!(!handle.is_cancelled());
        assert!(!cloned.is_cancelled());

        handle.cancel();
        assert!(handle.is_cancelled());
        assert!(cloned.is_cancelled()); // 共享同一个 AtomicBool
    }

    #[test]
    fn test_cancellation_handle_default() {
        let handle = CancellationHandle::default();
        assert!(!handle.is_cancelled());
    }

    #[test]
    fn test_content_block_serialization() {
        let block = ContentBlock::Text { text: "Hello".into() };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ContentBlock::Text { .. }));

        let block = ContentBlock::ToolUse {
            id: "tool-1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "ls"}),
        };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ContentBlock::ToolUse { .. }));

        let block = ContentBlock::Thinking { thinking: "Let me reason...".into() };
        let json = serde_json::to_string(&block).unwrap();
        let deserialized: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ContentBlock::Thinking { .. }));
    }

    #[test]
    fn test_tool_choice_serialization() {
        let tc = ToolChoice::Auto;
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ToolChoice::Auto));

        let tc = ToolChoice::Required;
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ToolChoice::Required));

        let tc = ToolChoice::None;
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ToolChoice::None));

        let tc = ToolChoice::Specific { name: "bash".into() };
        let json = serde_json::to_string(&tc).unwrap();
        let deserialized: ToolChoice = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, ToolChoice::Specific { .. }));
    }

    #[test]
    fn test_token_usage_equality() {
        let u1 = TokenUsage { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 };
        let u2 = TokenUsage { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 };
        assert_eq!(u1, u2);
    }

    #[test]
    fn test_stream_chunk_with_usage() {
        let chunk = StreamChunk {
            request_id: "req-1".into(),
            channel: StreamChannel::Text,
            content: "Hello".into(),
            tool_call: None,
            usage: Some(TokenUsage { prompt_tokens: 5, completion_tokens: 3, total_tokens: 8 }),
        };
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 5);
    }
}
