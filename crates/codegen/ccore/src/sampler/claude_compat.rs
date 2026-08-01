//! Anthropic Claude Messages API 兼容适配器
//!
//! 适用于：Anthropic Claude 系列模型（claude-sonnet-4-20250514 等）
//!
//! 流式解析基于 SSE (Server-Sent Events) 协议：
//! - 每行以 "event: " 标识事件类型，"data: " 携带 JSON 数据
//! - 事件类型：message_start / content_block_start / content_block_delta / message_stop
//!
//! Anthropic Messages API 流式事件序列：
//! 1. message_start    - 消息开始，包含 message 元信息
//! 2. content_block_start - 内容块开始（text / tool_use）
//! 3. content_block_delta - 内容块增量（text_delta / input_json_delta）
//! 4. content_block_stop  - 内容块结束
//! 5. message_delta    - 消息级增量（stop_reason / usage）
//! 6. message_stop     - 消息结束
//!
//! 关键约束：
//! - 使用 x-api-key 头认证（非 Bearer token）
//! - 需要 anthropic-version 头指定 API 版本
//! - 工具调用在 content_block_delta 中 type: input_json_delta

use anyhow::{anyhow, Result};
use futures::stream::StreamExt;
use futures::Stream;
use serde::Deserialize;
use std::pin::Pin;

use super::provider::{
    Provider, SampleRequest, SampleResponse, StreamChannel, StreamChunk, TokenUsage, ToolCallChunk,
    CancellationHandle,
};

/// Claude 兼容 Provider 配置
#[derive(Debug, Clone)]
pub struct ClaudeCompatConfig {
    /// Provider 名称
    pub name: String,
    /// Anthropic API Key
    pub api_key: String,
    /// API 基础 URL（默认 https://api.anthropic.com）
    pub base_url: String,
    /// 支持的模型列表
    pub models: Vec<String>,
    /// Anthropic API 版本（默认 "2023-06-01"）
    pub api_version: String,
}

/// Claude 兼容适配器
pub struct ClaudeCompatProvider {
    config: ClaudeCompatConfig,
    client: reqwest::Client,
}

impl ClaudeCompatProvider {
    pub fn new(config: ClaudeCompatConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 构造 Messages API 请求 URL
    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// 将 SampleRequest 转换为 Anthropic Messages API 请求体
    fn build_request_body(&self, request: &SampleRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": request.messages.iter().map(|m| {
                // Anthropic 要求 content 为字符串或内容块数组
                serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                })
            }).collect::<Vec<_>>(),
            "stream": request.stream,
            // Anthropic 要求必须设置 max_tokens
            "max_tokens": request.max_tokens.unwrap_or(8192),
        });

        // 系统提示作为顶层 system 字段（而非放在 messages 中）
        if let Some(ref system_prompt) = request.system_prompt {
            body["system"] = serde_json::json!(system_prompt);
        }

        // 工具定义
        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(
                request.tools.iter().map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    })
                }).collect::<Vec<_>>()
            );

            // 工具选择策略
            if let Some(ref tool_choice) = request.tool_choice {
                body["tool_choice"] = match tool_choice {
                    super::provider::ToolChoice::Auto => serde_json::json!({"type": "auto"}),
                    super::provider::ToolChoice::Required => serde_json::json!({"type": "any"}),
                    super::provider::ToolChoice::None => serde_json::json!({"type": "none"}),
                    super::provider::ToolChoice::Specific { name } => serde_json::json!({
                        "type": "tool",
                        "name": name
                    }),
                };
            }
        }

        // 温度
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        body
    }

    /// 解析 SSE 事件行，提取 event 和 data
    /// Anthropic SSE 格式：
    ///   event: message_start
    ///   data: {"type":"message_start",...}
    fn parse_sse_event(
        request_id: &str,
        event_type: &str,
        data: &str,
        // 状态：当前正在处理的工具调用（content_block_start 时创建，content_block_stop 时清空）
        active_tool: &mut Option<ActiveToolCall>,
    ) -> Vec<Result<StreamChunk>> {
        let mut results = Vec::new();

        match event_type {
            "content_block_delta" => {
                // 内容块增量事件
                tracing::trace!(
                    target: "ccore::sampler",
                    provider = "claude",
                    chunk_type = "content_block_delta",
                    "SSE chunk"
                );
                let delta: ClaudeContentBlockDelta = match serde_json::from_str(data) {
                    Ok(d) => d,
                    Err(e) => {
                        results.push(Err(anyhow!("content_block_delta JSON 解析失败：{}", e)));
                        return results;
                    }
                };

                match delta.delta.r#type.as_str() {
                    "text_delta" => {
                        // 文本增量
                        if !delta.delta.text.is_empty() {
                            results.push(Ok(StreamChunk {
                                request_id: request_id.to_string(),
                                channel: StreamChannel::Text,
                                content: delta.delta.text,
                                tool_call: None,
                                usage: None,
                            }));
                        }
                    }
                    "input_json_delta" => {
                        // 工具调用参数增量
                        if let Some(tool) = active_tool {
                            results.push(Ok(StreamChunk {
                                request_id: request_id.to_string(),
                                channel: StreamChannel::ToolCall,
                                content: delta.delta.partial_json.clone(),
                                tool_call: Some(ToolCallChunk {
                                    tool_call_id: tool.tool_call_id.clone(),
                                    tool_name: tool.tool_name.clone(),
                                    arguments: delta.delta.partial_json,
                                }),
                                usage: None,
                            }));
                        }
                    }
                    "thinking_delta" => {
                        // 思维链增量（扩展思维模式）
                        if !delta.delta.thinking.is_empty() {
                            results.push(Ok(StreamChunk {
                                request_id: request_id.to_string(),
                                channel: StreamChannel::Reasoning,
                                content: delta.delta.thinking,
                                tool_call: None,
                                usage: None,
                            }));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_start" => {
                // 内容块开始事件 - 记录工具调用信息
                let block_start: ClaudeContentBlockStart = match serde_json::from_str(data) {
                    Ok(b) => b,
                    Err(e) => {
                        results.push(Err(anyhow!("content_block_start JSON 解析失败：{}", e)));
                        return results;
                    }
                };

                if block_start.content_block.r#type == "tool_use" {
                    // 记录当前活跃的工具调用
                    *active_tool = Some(ActiveToolCall {
                        tool_call_id: block_start.content_block.id,
                        tool_name: block_start.content_block.name,
                    });
                } else if block_start.content_block.r#type == "thinking" {
                    // Extended thinking 块开始 - 不需要特殊处理，后续 thinking_delta 会发送内容
                }
            }
            "content_block_stop" => {
                // 内容块结束 - 清除活跃工具调用
                *active_tool = None;
            }
            "message_start" => {
                // 消息开始 - 包含初始 usage 信息
                tracing::trace!(
                    target: "ccore::sampler",
                    provider = "claude",
                    chunk_type = "message_start",
                    "SSE chunk"
                );
                let msg_start: ClaudeMessageStart = match serde_json::from_str(data) {
                    Ok(m) => m,
                    Err(e) => {
                        results.push(Err(anyhow!("message_start JSON 解析失败：{}", e)));
                        return results;
                    }
                };
                tracing::debug!(
                    target: "ccore::sampler",
                    provider = "claude",
                    input_tokens = msg_start.message.usage.input_tokens,
                    output_tokens = 0u32,
                    "token usage"
                );
                // 发送包含 usage 的 chunk
                results.push(Ok(StreamChunk {
                    request_id: request_id.to_string(),
                    channel: StreamChannel::Text,
                    content: String::new(),
                    tool_call: None,
                    usage: Some(TokenUsage {
                        prompt_tokens: msg_start.message.usage.input_tokens,
                        completion_tokens: 0,
                        total_tokens: msg_start.message.usage.input_tokens,
                    }),
                }));
            }
            "message_delta" => {
                // 消息级增量 - 包含 stop_reason 和 output_tokens usage
                tracing::trace!(
                    target: "ccore::sampler",
                    provider = "claude",
                    chunk_type = "message_delta",
                    "SSE chunk"
                );
                let msg_delta: ClaudeMessageDelta = match serde_json::from_str(data) {
                    Ok(m) => m,
                    Err(e) => {
                        results.push(Err(anyhow!("message_delta JSON 解析失败：{}", e)));
                        return results;
                    }
                };
                tracing::debug!(
                    target: "ccore::sampler",
                    provider = "claude",
                    input_tokens = 0u32,
                    output_tokens = msg_delta.usage.output_tokens,
                    "token usage"
                );
                // 发送包含 output usage 的 chunk
                results.push(Ok(StreamChunk {
                    request_id: request_id.to_string(),
                    channel: StreamChannel::Text,
                    content: String::new(),
                    tool_call: None,
                    usage: Some(TokenUsage {
                        prompt_tokens: 0,
                        completion_tokens: msg_delta.usage.output_tokens,
                        total_tokens: msg_delta.usage.output_tokens,
                    }),
                }));
            }
            // message_stop / ping 等事件不需要转换为 StreamChunk
            _ => {}
        }

        results
    }
}

/// 当前活跃的工具调用状态
#[derive(Debug, Clone)]
struct ActiveToolCall {
    tool_call_id: String,
    tool_name: String,
}

// ---- Anthropic Messages API 数据结构 ----

/// content_block_delta 事件
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeContentBlockDelta {
    delta: ClaudeDelta,
}

/// 增量内容
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeDelta {
    r#type: String,
    /// 文本增量
    #[serde(default)]
    text: String,
    /// 工具调用 JSON 增量
    #[serde(default)]
    partial_json: String,
    /// 思维链增量
    #[serde(default)]
    thinking: String,
}

/// content_block_start 事件
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeContentBlockStart {
    content_block: ClaudeContentBlock,
}

/// 内容块
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeContentBlock {
    r#type: String,
    /// 工具调用 ID（tool_use 类型）
    #[serde(default)]
    id: String,
    /// 工具名称（tool_use 类型）
    #[serde(default)]
    name: String,
}

/// message_start 事件
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeMessageStart {
    message: ClaudeMessage,
}

/// Anthropic Message 对象
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeMessage {
    id: String,
    r#type: String,
    role: String,
    content: Vec<serde_json::Value>,
    model: String,
    stop_reason: Option<String>,
    usage: ClaudeUsage,
}

/// Token 使用量
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeUsage {
    input_tokens: u32,
    output_tokens: u32,
}

/// message_delta 事件
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeMessageDelta {
    delta: ClaudeMessageDeltaData,
    usage: ClaudeMessageDeltaUsage,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeMessageDeltaData {
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeMessageDeltaUsage {
    output_tokens: u32,
}

/// Anthropic 非流式响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeResponse {
    id: String,
    r#type: String,
    role: String,
    content: Vec<ClaudeResponseContentBlock>,
    model: String,
    stop_reason: Option<String>,
    usage: ClaudeUsage,
}

/// 非流式响应内容块
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeResponseContentBlock {
    r#type: String,
    /// text 类型的文本内容
    #[serde(default)]
    text: String,
    /// tool_use 类型的工具调用 ID
    #[serde(default)]
    id: String,
    /// tool_use 类型的工具名称
    #[serde(default)]
    name: String,
    /// tool_use 类型的工具输入参数
    #[serde(default)]
    input: serde_json::Value,
}

#[async_trait::async_trait]
impl Provider for ClaudeCompatProvider {
    async fn stream(
        &self,
        request: SampleRequest,
        cancel: CancellationHandle,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        let body = self.build_request_body(&request);
        let request_id = request.request_id.clone();

        tracing::debug!(
            target: "ccore::sampler",
            provider = %self.config.name,
            model = %request.model,
            "sampling request"
        );

        let response = self
            .client
            .post(self.messages_url())
            // Anthropic 使用 x-api-key 头认证
            .header("x-api-key", &self.config.api_key)
            // 必须指定 API 版本
            .header("anthropic-version", &self.config.api_version)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("HTTP 请求失败：{}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                tracing::warn!(
                    target: "ccore::sampler",
                    provider = %self.config.name,
                    "rate limit hit"
                );
            }
            tracing::error!(
                target: "ccore::sampler",
                provider = %self.config.name,
                error = %format!("API returned error {}: {}", status, body_text.chars().take(200).collect::<String>()),
                "sampling request failed"
            );
            return Err(anyhow!("API 返回错误 {}：{}", status, body_text));
        }

        // 将 response body 转为字节流，按 SSE 行解析
        let byte_stream = response.bytes_stream();

        // 使用 buffer + 行分割解析 SSE
        // Anthropic SSE 格式：
        //   event: message_start
        //   data: {...}
        //   （空行分隔事件）
        let rid = request_id.clone();
        let stream = byte_stream
            .scan(
                SseParserState::default(),
                move |state, chunk_result| {
                    // 检查取消信号
                    if cancel.is_cancelled() {
                        return std::future::ready(None);
                    }

                    let chunk = match chunk_result {
                        Ok(c) => c,
                        Err(e) => {
                            return std::future::ready(Some(vec![Err(anyhow!(
                                "流读取错误：{}",
                                e
                            ))]));
                        }
                    };

                    state.buffer.push_str(&String::from_utf8_lossy(&chunk));

                    let mut results = Vec::new();

                    // 按行分割，累积 event 和 data
                    while let Some(pos) = state.buffer.find('\n') {
                        let line = state.buffer[..pos].trim().to_string();
                        state.buffer.drain(..=pos);

                        if let Some(stripped) = line.strip_prefix("event: ") {
                            state.current_event = stripped.to_string();
                        } else if let Some(stripped) = line.strip_prefix("data: ") {
                            state.current_data = stripped.to_string();

                            // event + data 都已收到，解析事件
                            if !state.current_event.is_empty() {
                                let chunks = Self::parse_sse_event(
                                    &rid,
                                    &state.current_event,
                                    &state.current_data,
                                    &mut state.active_tool,
                                );
                                results.extend(chunks);
                                state.current_event.clear();
                                state.current_data.clear();
                            }
                        } else if line.is_empty() {
                        // 空行表示事件结束，重置状态
                        state.current_event.clear();
                        state.current_data.clear();
                        }
                    }

                    std::future::ready(Some(results))
                },
            )
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }

    async fn sample(&self, request: SampleRequest) -> Result<SampleResponse> {
        let mut body = self.build_request_body(&request);
        body["stream"] = serde_json::json!(false);

        let response = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", &self.config.api_version)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("HTTP 请求失败：{}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            if status.as_u16() == 429 {
                tracing::warn!(
                    target: "ccore::sampler",
                    provider = %self.config.name,
                    "rate limit hit"
                );
            }
            tracing::error!(
                target: "ccore::sampler",
                provider = %self.config.name,
                error = %format!("API returned error {}: {}", status, body_text.chars().take(200).collect::<String>()),
                "sampling request failed"
            );
            return Err(anyhow!("API 返回错误 {}：{}", status, body_text));
        }

        let resp: ClaudeResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("响应 JSON 解析失败：{}", e))?;

        Ok(SampleResponse {
            request_id: request.request_id,
            usage: TokenUsage {
                prompt_tokens: resp.usage.input_tokens,
                completion_tokens: resp.usage.output_tokens,
                total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            },
            finish_reason: resp.stop_reason.unwrap_or_default(),
        })
    }

    fn model_list(&self) -> &[String] {
        &self.config.models
    }

    fn name(&self) -> &str {
        &self.config.name
    }
}

/// SSE 解析器状态
#[derive(Default)]
struct SseParserState {
    /// 行缓冲区
    buffer: String,
    /// 当前事件类型
    current_event: String,
    /// 当前事件数据
    current_data: String,
    /// 当前活跃的工具调用
    active_tool: Option<ActiveToolCall>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_delta() {
        let mut active_tool = None;
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tool);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert_eq!(chunk.request_id, "req-1");
        assert!(matches!(chunk.channel, StreamChannel::Text));
        assert_eq!(chunk.content, "Hello");
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_parse_tool_use_start() {
        let mut active_tool = None;
        let data = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"bash"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_start", data, &mut active_tool);
        // content_block_start 不产生 StreamChunk，但应记录活跃工具调用
        assert!(chunks.is_empty());
        assert!(active_tool.is_some());
        let tool = active_tool.unwrap();
        assert_eq!(tool.tool_call_id, "toolu_abc");
        assert_eq!(tool.tool_name, "bash");
    }

    #[test]
    fn test_parse_input_json_delta() {
        let mut active_tool = Some(ActiveToolCall {
            tool_call_id: "toolu_abc".into(),
            tool_name: "bash".into(),
        });
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tool);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::ToolCall));
        let tc = chunk.tool_call.as_ref().unwrap();
        assert_eq!(tc.tool_name, "bash");
        assert_eq!(tc.tool_call_id, "toolu_abc");
    }

    #[test]
    fn test_parse_thinking_delta() {
        let mut active_tool = None;
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tool);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::Reasoning));
        assert_eq!(chunk.content, "Let me think...");
    }

    #[test]
    fn test_build_request_body() {
        let config = ClaudeCompatConfig {
            name: "claude".into(),
            api_key: "sk-ant-test".into(),
            base_url: "https://api.anthropic.com".into(),
            models: vec!["claude-sonnet-4-20250514".into()],
            api_version: "2023-06-01".into(),
        };
        let provider = ClaudeCompatProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "claude-sonnet-4-20250514".into(),
            messages: vec![super::super::provider::ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
                cache_control: None,
            }],
            tools: Vec::new(),
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            tool_choice: None,
            prompt_cache_key: None,
        };

        let body = provider.build_request_body(&request);
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["stream"], true);
        // max_tokens 默认 8192
        assert_eq!(body["max_tokens"], 8192);
        // 无 system prompt 时不应该有 system 字段
        assert!(body.get("system").is_none());
    }

    #[test]
    fn test_build_request_body_with_tools() {
        let config = ClaudeCompatConfig {
            name: "claude".into(),
            api_key: "sk-ant-test".into(),
            base_url: "https://api.anthropic.com".into(),
            models: vec!["claude-sonnet-4-20250514".into()],
            api_version: "2023-06-01".into(),
        };
        let provider = ClaudeCompatProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "claude-sonnet-4-20250514".into(),
            messages: vec![super::super::provider::ChatMessage {
                role: "user".into(),
                content: "List files".into(),
                cache_control: None,
            }],
            tools: vec![super::super::provider::ToolDefinition {
                name: "bash".into(),
                description: "Execute bash commands".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            }],
            stream: false,
            reasoning_effort: None,
            max_tokens: Some(4096),
            temperature: Some(0.5),
            system_prompt: Some("You are a helpful assistant.".into()),
            tool_choice: Some(super::super::provider::ToolChoice::Auto),
            prompt_cache_key: None,
        };

        let body = provider.build_request_body(&request);
        // 工具使用 input_schema 而非 parameters
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].get("input_schema").is_some());
        assert!(tools[0].get("parameters").is_none());
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["temperature"], 0.5);
        // 系统提示作为顶层 system 字段
        assert_eq!(body["system"], "You are a helpful assistant.");
        // 工具选择
        assert_eq!(body["tool_choice"]["type"], "auto");
    }

    #[test]
    fn test_parse_message_start_with_usage() {
        let mut active_tool = None;
        let data = r#"{"type":"message_start","message":{"id":"msg_abc","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"usage":{"input_tokens":25,"output_tokens":0}}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "message_start", data, &mut active_tool);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 25);
        assert_eq!(usage.completion_tokens, 0);
    }

    #[test]
    fn test_parse_message_delta_with_usage() {
        let mut active_tool = None;
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "message_delta", data, &mut active_tool);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.completion_tokens, 15);
    }
}
