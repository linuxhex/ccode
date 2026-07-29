//! DeepSeek API 兼容适配器
//!
//! 适用于：DeepSeek 系列模型（deepseek-chat / deepseek-reasoner）
//!
//! DeepSeek API 基于 OpenAI Chat Completions 格式，但有独特扩展：
//! - deepseek-reasoner 在 delta 中同时返回 `content` 和 `reasoning_content`
//! - `reasoning_content` 包含思维链内容，映射到 StreamChannel::Reasoning
//! - deepseek-chat 使用标准 OpenAI 格式
//! - API 端点：https://api.deepseek.com/v1/chat/completions
//!
//! 流式解析基于 SSE (Server-Sent Events) 协议：
//! - 每行以 "data: " 开头
//! - 内容为 JSON 格式的 chunk
//! - 流结束标记为 "data: [DONE]"

use anyhow::{anyhow, Result};
use futures::stream::StreamExt;
use futures::Stream;
use serde::Deserialize;
use std::pin::Pin;

use super::provider::{
    Provider, SampleRequest, SampleResponse, StreamChunk, StreamChannel,
    TokenUsage, ToolCallChunk, CancellationHandle,
};

/// DeepSeek Provider 配置
#[derive(Debug, Clone)]
pub struct DeepSeekCompatConfig {
    /// Provider 名称
    pub name: String,
    /// DeepSeek API Key
    pub api_key: String,
    /// API 基础 URL（默认 https://api.deepseek.com）
    pub base_url: String,
    /// 支持的模型列表
    pub models: Vec<String>,
}

/// DeepSeek 兼容适配器
pub struct DeepSeekCompatProvider {
    config: DeepSeekCompatConfig,
    client: reqwest::Client,
}

impl DeepSeekCompatProvider {
    pub fn new(config: DeepSeekCompatConfig) -> Self {
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

    /// 判断是否为推理模型（deepseek-reasoner）
    #[allow(dead_code)]  // 保留为 API，供多模型路由使用
    fn is_reasoner_model(model: &str) -> bool {
        model.contains("reasoner") || model.contains("r1")
    }

    /// 将 SampleRequest 转换为 DeepSeek API 请求体
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
    /// DeepSeek 的 reasoning_content 在 delta 中与 content 并列
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
        let chunk: DeepSeekStreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                return Some(Err(anyhow!("SSE chunk JSON 解析失败：{}", e)));
            }
        };

        // 提取 usage（DeepSeek 在最后一个 chunk 或 stream_options 开启时返回 usage）
        let usage = chunk.usage.map(|u| TokenUsage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        // 提取第一个 choice 的 delta
        let choice = chunk.choices.first()?;

        // 转换 delta 为 StreamChunk
        let delta = &choice.delta;

        // 优先处理 reasoning_content（DeepSeek reasoner 特有）
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

        // 普通文本内容
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

// ---- DeepSeek API 数据结构 ----

/// DeepSeek 流式 chunk 原始格式（与 OpenAI 兼容，但含 reasoning_content）
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekStreamChunk {
    choices: Vec<DeepSeekChoice>,
    usage: Option<DeepSeekUsage>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekChoice {
    index: u32,
    delta: DeepSeekDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekDelta {
    role: Option<String>,
    content: Option<String>,
    /// DeepSeek 特有：推理内容（deepseek-reasoner 返回）
    #[serde(default)]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeepSeekToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekToolCallDelta {
    index: u32,
    id: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    function: Option<DeepSeekFunctionDelta>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekFunctionDelta {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

/// DeepSeek 非流式响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekResponse {
    id: String,
    choices: Vec<DeepSeekResponseChoice>,
    usage: DeepSeekUsage,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekResponseChoice {
    index: u32,
    message: DeepSeekResponseMessage,
    finish_reason: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekResponseMessage {
    role: String,
    content: Option<String>,
    /// DeepSeek 特有：推理内容
    #[serde(default)]
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeepSeekResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekResponseToolCall {
    id: String,
    r#type: String,
    function: DeepSeekResponseFunction,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct DeepSeekResponseFunction {
    name: String,
    arguments: String,
}

#[async_trait::async_trait]
impl Provider for DeepSeekCompatProvider {
    async fn stream(
        &self,
        request: SampleRequest,
        cancel: CancellationHandle,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        let body = self.build_request_body(&request);
        let request_id = request.request_id.clone();

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
            return Err(anyhow!("API 返回错误 {}：{}", status, body));
        }

        let byte_stream = response.bytes_stream();

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

        let resp: DeepSeekResponse = response
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
        let chunk = DeepSeekCompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert_eq!(chunk.request_id, "req-1");
        assert!(matches!(chunk.channel, StreamChannel::Text));
        assert_eq!(chunk.content, "Hello");
    }

    #[test]
    fn test_parse_sse_reasoning_content() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"Let me reason through this..."},"finish_reason":null}]}"#;
        let chunk = DeepSeekCompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert!(matches!(chunk.channel, StreamChannel::Reasoning));
        assert_eq!(chunk.content, "Let me reason through this...");
    }

    #[test]
    fn test_parse_sse_reasoning_before_content() {
        // DeepSeek reasoner 先发 reasoning_content，再发 content
        let line1 = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"Step 1: Analyze"},"finish_reason":null}]}"#;
        let chunk1 = DeepSeekCompatProvider::parse_sse_line("req-1", line1)
            .unwrap()
            .unwrap();
        assert!(matches!(chunk1.channel, StreamChannel::Reasoning));

        let line2 = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"The answer is"},"finish_reason":null}]}"#;
        let chunk2 = DeepSeekCompatProvider::parse_sse_line("req-1", line2)
            .unwrap()
            .unwrap();
        assert!(matches!(chunk2.channel, StreamChannel::Text));
    }

    #[test]
    fn test_parse_sse_done() {
        let line = "data: [DONE]";
        assert!(DeepSeekCompatProvider::parse_sse_line("req-1", line).is_none());
    }

    #[test]
    fn test_parse_sse_with_usage() {
        let line = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"reasoning_content":"thinking..."},"finish_reason":null}],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#;
        let chunk = DeepSeekCompatProvider::parse_sse_line("req-1", line)
            .unwrap()
            .unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn test_is_reasoner_model() {
        assert!(DeepSeekCompatProvider::is_reasoner_model("deepseek-reasoner"));
        assert!(DeepSeekCompatProvider::is_reasoner_model("deepseek-r1"));
        assert!(!DeepSeekCompatProvider::is_reasoner_model("deepseek-chat"));
    }

    #[test]
    fn test_build_request_body_with_system_prompt() {
        let config = DeepSeekCompatConfig {
            name: "deepseek".into(),
            api_key: "sk-test".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            models: vec!["deepseek-reasoner".into()],
        };
        let provider = DeepSeekCompatProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "deepseek-reasoner".into(),
            messages: vec![super::super::provider::ChatMessage {
                role: "user".into(),
                content: "Hello".into(),
            }],
            tools: Vec::new(),
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
            system_prompt: Some("You are a coding assistant.".into()),
            tool_choice: None,
        };

        let body = provider.build_request_body(&request);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are a coding assistant.");
        assert_eq!(messages[1]["role"], "user");
    }
}
