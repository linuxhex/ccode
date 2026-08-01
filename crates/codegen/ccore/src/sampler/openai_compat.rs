//! OpenAI Chat Completions 兼容适配器
//!
//! 适用于：OpenAI、DeepSeek、Qoder、ccode Ccode 等原生兼容 /v1/chat/completions 的后端
//!
//! 流式解析基于 SSE (Server-Sent Events) 协议：
//! - 每行以 "data: " 开头
//! - 内容为 JSON 格式的 chunk
//! - 流结束标记为 "data: [DONE]"
//!
//! OpenAI 流式 chunk 格式：
//! ```json
//! {
//!   "id": "chatcmpl-xxx",
//!   "choices": [{
//!     "index": 0,
//!     "delta": { "role": "assistant" | "content": "..." | "tool_calls": [...] },
//!     "finish_reason": null | "stop" | "tool_calls"
//!   }],
//!   "usage": { "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0 }
//! }
//! ```

use anyhow::{Result, anyhow};
use futures::stream::StreamExt;
use futures::Stream;
use serde::Deserialize;
use std::pin::Pin;

use super::provider::{
    Provider, SampleRequest, SampleResponse, StreamChunk, StreamChannel,
    TokenUsage, ToolCallChunk, CancellationHandle,
};

/// OpenAI 兼容 Provider 配置
#[derive(Debug, Clone)]
pub struct OpenAICompatConfig {
    pub name: String,
    pub api_key: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// OpenAI 兼容适配器
pub struct OpenAICompatProvider {
    config: OpenAICompatConfig,
    client: reqwest::Client,
}

impl OpenAICompatProvider {
    pub fn new(config: OpenAICompatConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 构造请求 URL
    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }

    /// 将 SampleRequest 转换为 OpenAI API 请求体
    fn build_request_body(&self, request: &SampleRequest) -> serde_json::Value {
        // 构建消息列表：如果有 system_prompt，将其插入为第一条 system 消息
        let mut messages = Vec::new();
        if let Some(ref system_prompt) = request.system_prompt {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system_prompt,
            }));
        }
        for m in &request.messages {
            messages.push(serde_json::json!({
                "role": m.role,
                "content": m.content,
            }));
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "stream": request.stream,
        });

        if !request.tools.is_empty() {
            body["tools"] = serde_json::json!(
                request.tools.iter().map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                }).collect::<Vec<_>>()
            );

            // 工具选择策略
            if let Some(ref tool_choice) = request.tool_choice {
                body["tool_choice"] = match tool_choice {
                    super::provider::ToolChoice::Auto => serde_json::json!("auto"),
                    super::provider::ToolChoice::Required => serde_json::json!("required"),
                    super::provider::ToolChoice::None => serde_json::json!("none"),
                    super::provider::ToolChoice::Specific { name } => serde_json::json!({
                        "type": "function",
                        "function": { "name": name }
                    }),
                };
            }
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        body
    }

    /// 解析 SSE 行，提取 data 部分并转换为 StreamChunk
    fn parse_sse_line(request_id: &str, line: &str) -> Option<Result<StreamChunk>> {
        let line = line.trim();

        // 跳过空行和注释
        if line.is_empty() || line.starts_with(':') {
            return None;
        }

        // 提取 data: 后的内容
        let data = line.strip_prefix("data: ")?;

        // 流结束标记
        if data == "[DONE]" {
            return None;
        }

        // 解析 JSON
        let chunk: OpenAIStreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                return Some(Err(anyhow!("SSE chunk JSON 解析失败：{}", e)));
            }
        };

        // 提取 usage（某些 API 在最后一个 chunk 中返回 usage）
        let usage = chunk.usage.map(|u| {
            tracing::debug!(
                target: "ccore::sampler",
                provider = "openai",
                input_tokens = u.prompt_tokens,
                output_tokens = u.completion_tokens,
                "token usage"
            );
            TokenUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }
        });

        tracing::trace!(
            target: "ccore::sampler",
            provider = "openai",
            chunk_type = "delta",
            "SSE chunk"
        );

        // 提取第一个 choice 的 delta
        let choice = chunk.choices.first()?;

        // 转换 delta 为 StreamChunk
        let delta = &choice.delta;

        if let Some(content) = &delta.content {
            if !content.is_empty() {
                return Some(Ok(StreamChunk {
                    request_id: request_id.to_string(),
                    channel: StreamChannel::Text,
                    content: content.clone(),
                    tool_call: None,
                    usage: usage.clone(),
                }));
            }
        }

        // 推理内容（某些模型支持）
        if let Some(reasoning_content) = &delta.reasoning_content {
            if !reasoning_content.is_empty() {
                return Some(Ok(StreamChunk {
                    request_id: request_id.to_string(),
                    channel: StreamChannel::Reasoning,
                    content: reasoning_content.clone(),
                    tool_call: None,
                    usage: usage.clone(),
                }));
            }
        }

        // 工具调用
        if let Some(tool_calls) = &delta.tool_calls {
            for tc in tool_calls {
                let tool_call_id = tc.id.clone().unwrap_or_default();
                let tool_name = tc.function
                    .as_ref()
                    .map(|f| f.name.clone())
                    .unwrap_or_default();
                let arguments = tc.function
                    .as_ref()
                    .map(|f| f.arguments.clone())
                    .unwrap_or_default();

                if !tool_name.is_empty() {
                    return Some(Ok(StreamChunk {
                        request_id: request_id.to_string(),
                        channel: StreamChannel::ToolCall,
                        content: arguments.clone(),
                        tool_call: Some(ToolCallChunk {
                            tool_call_id,
                            tool_name,
                            arguments,
                        }),
                        usage: usage.clone(),
                    }));
                }
            }
        }

        None
    }
}

// ---- OpenAI API 数据结构 ----

/// OpenAI 流式 chunk 原始格式
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIStreamChunk {
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    index: u32,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIDelta {
    role: Option<String>,
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIToolCallDelta {
    index: u32,
    id: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIFunctionDelta {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// OpenAI 非流式响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    id: String,
    choices: Vec<OpenAIResponseChoice>,
    usage: OpenAIUsage,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponseChoice {
    index: u32,
    message: OpenAIResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponseMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponseToolCall {
    id: String,
    r#type: String,
    function: OpenAIResponseFunction,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponseFunction {
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl Provider for OpenAICompatProvider {
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

        let response = self.client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("HTTP 请求失败：{}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
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
                error = %format!("API returned error {}: {}", status, body.chars().take(200).collect::<String>()),
                "sampling request failed"
            );
            return Err(anyhow!("API 返回错误 {}：{}", status, body));
        }

        // 将 response body 转为字节流，然后按 SSE 行解析
        let byte_stream = response.bytes_stream();

        // 使用 buffer + 行分割解析 SSE
        let rid = request_id.clone();
        let stream = byte_stream
            .scan(String::new(), move |buffer, chunk_result| {
                // 检查取消信号
                if cancel.is_cancelled() {
                    return std::future::ready(None);
                }

                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        return std::future::ready(Some(vec![Err(anyhow!("流读取错误：{}", e))]));
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                let mut results = Vec::new();

                // 按行分割
                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].to_string();
                    buffer.drain(..=pos);

                    if let Some(chunk) = Self::parse_sse_line(&rid, &line) {
                        results.push(chunk);
                    }
                }

                std::future::ready(Some(results))
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }

    async fn sample(&self, request: SampleRequest) -> Result<SampleResponse> {
        let mut body = self.build_request_body(&request);
        body["stream"] = serde_json::json!(false);

        let response = self.client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("HTTP 请求失败：{}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("API 返回错误 {}：{}", status, body_text));
        }

        let resp: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("响应 JSON 解析失败：{}", e))?;

        let usage = resp.usage;

        Ok(SampleResponse {
            request_id: request.request_id,
            usage: TokenUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
            },
            finish_reason: resp.choices
                .first()
                .map(|c| c.finish_reason.clone())
                .unwrap_or_default(),
        })
    }

    fn model_list(&self) -> &[String] {
        &self.config.models
    }

    fn name(&self) -> &str {
        &self.config.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_text_chunk() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let chunk = OpenAICompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.request_id, "req-1");
        assert!(matches!(chunk.channel, StreamChannel::Text));
        assert_eq!(chunk.content, "Hello");
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_parse_sse_done() {
        let line = "data: [DONE]";
        assert!(OpenAICompatProvider::parse_sse_line("req-1", line).is_none());
    }

    #[test]
    fn test_parse_sse_tool_call() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]},"finish_reason":null}]}"#;
        let chunk = OpenAICompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert!(matches!(chunk.channel, StreamChannel::ToolCall));
        assert!(chunk.tool_call.is_some());
        let tc = chunk.tool_call.unwrap();
        assert_eq!(tc.tool_name, "bash");
    }

    #[test]
    fn test_build_request_body() {
        let config = OpenAICompatConfig {
            name: "test".into(),
            api_key: "key".into(),
            base_url: "https://api.ccode.dev/v1".into(),
            models: vec!["ccode-3".into()],
        };
        let provider = OpenAICompatProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "ccode-3".into(),
            messages: vec![
                super::super::provider::ChatMessage {
                    role: "user".into(),
                    content: "Hello".into(),
                    cache_control: None,
                },
            ],
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
        assert_eq!(body["model"], "ccode-3");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn test_parse_sse_reasoning_chunk() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"Let me think..."},"finish_reason":null}]}"#;
        let chunk = OpenAICompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert!(matches!(chunk.channel, StreamChannel::Reasoning));
        assert_eq!(chunk.content, "Let me think...");
    }

    #[test]
    fn test_parse_sse_chunk_with_usage() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        // delta content is empty, so this should return None (no content to emit)
        assert!(OpenAICompatProvider::parse_sse_line("req-1", line).is_none());
    }

    #[test]
    fn test_build_request_body_with_system_prompt_and_tool_choice() {
        let config = OpenAICompatConfig {
            name: "test".into(),
            api_key: "key".into(),
            base_url: "https://api.ccode.dev/v1".into(),
            models: vec!["ccode-3".into()],
        };
        let provider = OpenAICompatProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "ccode-3".into(),
            messages: vec![
                super::super::provider::ChatMessage {
                    role: "user".into(),
                    content: "Hello".into(),
                    cache_control: None,
                },
            ],
            tools: vec![super::super::provider::ToolDefinition {
                name: "bash".into(),
                description: "Execute bash".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
            system_prompt: Some("You are a helpful assistant.".into()),
            tool_choice: Some(super::super::provider::ToolChoice::Auto),
            prompt_cache_key: None,
        };

        let body = provider.build_request_body(&request);
        // 系统提示应作为第一条 system 消息
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a helpful assistant.");
        assert_eq!(messages[1]["role"], "user");
        // 工具选择
        assert_eq!(body["tool_choice"], "auto");
    }
}
