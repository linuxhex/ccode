//! Google Gemini API 适配器
//!
//! 适用于：Gemini 2.5 Pro/Flash、Gemini 2.0 Flash 等
//!
//! Gemini API 与 OpenAI 格式不同：
//! - 端点：POST /v1beta/models/{model}:streamGenerateContent?alt=sse
//! - 认证：x-goog-api-key 头（或 ?key= 查询参数）
//! - 系统提示：顶层 systemInstruction 字段
//! - 工具定义：functionDeclarations 格式
//! - 工具调用：candidates[].content.parts[].functionCall
//! - 思维链：parts[].thought（Gemini 2.5 支持）
//!
//! 流式返回格式（SSE）：
//!   data: {"candidates": [{"content": {"parts": [{"text": "..."}], "role": "model"}}]}
//!
//! 参考：https://ai.google.dev/gemini-api/docs

use anyhow::{anyhow, Result};
use futures::stream::StreamExt;
use futures::Stream;
use serde::Deserialize;
use std::pin::Pin;

use super::provider::{
    Provider, SampleRequest, SampleResponse, StreamChannel, StreamChunk, TokenUsage, ToolCallChunk,
    CancellationHandle,
};

/// Gemini Provider 配置
#[derive(Debug, Clone)]
pub struct GeminiConfig {
    pub name: String,
    pub api_key: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Gemini 适配器
pub struct GeminiProvider {
    config: GeminiConfig,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(config: GeminiConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 构造流式端点 URL
    fn stream_url(&self, model: &str) -> String {
        format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.config.base_url.trim_end_matches('/'),
            model
        )
    }

    /// 非流式端点
    fn generate_url(&self, model: &str) -> String {
        format!(
            "{}/v1beta/models/{}:generateContent",
            self.config.base_url.trim_end_matches('/'),
            model
        )
    }

    /// 将 SampleRequest 转换为 Gemini API 请求体
    fn build_request_body(&self, request: &SampleRequest) -> serde_json::Value {
        let mut body = serde_json::json!({});

        // 系统提示：Gemini 使用顶层 systemInstruction
        if let Some(ref system_prompt) = request.system_prompt {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system_prompt}]
            });
        }

        // 构建 contents 数组
        let mut contents = Vec::new();
        for m in &request.messages {
            // Gemini 角色映射：assistant → model
            let role = match m.role.as_str() {
                "assistant" => "model",
                other => other,
            };
            contents.push(serde_json::json!({
                "role": role,
                "parts": [{"text": m.content}]
            }));
        }
        body["contents"] = serde_json::json!(contents);

        // 工具定义 → functionDeclarations
        if !request.tools.is_empty() {
            let function_declarations: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    })
                })
                .collect();

            body["tools"] = serde_json::json!([{
                "functionDeclarations": function_declarations
            }]);

            // 工具选择策略
            if let Some(ref tool_choice) = request.tool_choice {
                body["toolConfig"] = match tool_choice {
                    super::provider::ToolChoice::Auto => serde_json::json!({
                        "functionCallingConfig": {"mode": "AUTO"}
                    }),
                    super::provider::ToolChoice::Required => serde_json::json!({
                        "functionCallingConfig": {"mode": "ANY"}
                    }),
                    super::provider::ToolChoice::None => serde_json::json!({
                        "functionCallingConfig": {"mode": "NONE"}
                    }),
                    super::provider::ToolChoice::Specific { name } => serde_json::json!({
                        "functionCallingConfig": {
                            "mode": "ANY",
                            "allowedFunctionNames": [name]
                        }
                    }),
                };
            }
        }

        // 生成配置
        let mut generation_config = serde_json::json!({});
        if let Some(max_tokens) = request.max_tokens {
            generation_config["maxOutputTokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            generation_config["temperature"] = serde_json::json!(temperature);
        }
        if let Some(top_p) = request.top_p {
            generation_config["topP"] = serde_json::json!(top_p);
        }
        // Gemini thinking 配置
        if let Some(ref thinking) = request.thinking {
            if thinking.enabled {
                let budget = thinking.budget_tokens.unwrap_or(8192);
                generation_config["thinkingConfig"] = serde_json::json!({
                    "thinkingBudget": budget,
                });
            }
        }
        if generation_config.as_object().map_or(false, |o| !o.is_empty()) {
            body["generationConfig"] = generation_config;
        }

        body
    }

    /// 解析 SSE 事件 — Gemini 流式格式
    /// 每行格式：data: {"candidates": [...], "usageMetadata": {...}}
    fn parse_sse_data(request_id: &str, data: &str) -> Vec<Result<StreamChunk>> {
        let mut results = Vec::new();
        let data = data.trim();

        if data.is_empty() || data.starts_with(':') {
            return results;
        }

        let chunk: GeminiStreamChunk = match serde_json::from_str(data) {
            Ok(c) => c,
            Err(e) => {
                results.push(Err(anyhow!("Gemini SSE chunk JSON 解析失败：{}", e)));
                return results;
            }
        };

        // 提取 usage
        let usage = chunk.usage_metadata.map(|u| {
            tracing::debug!(
                target: "ccore::sampler",
                provider = "gemini",
                input_tokens = u.prompt_token_count,
                output_tokens = u.candidates_token_count,
                "token usage"
            );
            TokenUsage {
                prompt_tokens: u.prompt_token_count,
                completion_tokens: u.candidates_token_count,
                total_tokens: u.total_token_count,
            }
        });

        // 提取 candidates
        for candidate in &chunk.candidates {
            if let Some(content) = &candidate.content {
                for part in &content.parts {
                    // 文本内容
                    if let Some(text) = &part.text {
                        if !text.is_empty() {
                            results.push(Ok(StreamChunk {
                                request_id: request_id.to_string(),
                                channel: StreamChannel::Text,
                                content: text.clone(),
                                tool_call: None,
                                usage: usage.clone(),
                            }));
                        }
                    }

                    // 思维链（Gemini 2.5 thinking）
                    if let Some(thought) = &part.thought {
                        if !thought.is_empty() {
                            results.push(Ok(StreamChunk {
                                request_id: request_id.to_string(),
                                channel: StreamChannel::Reasoning,
                                content: thought.clone(),
                                tool_call: None,
                                usage: usage.clone(),
                            }));
                        }
                    }

                    // 工具调用
                    if let Some(func_call) = &part.function_call {
                        let args_str = serde_json::to_string(&func_call.args)
                            .unwrap_or_else(|_| "{}".to_string());
                        results.push(Ok(StreamChunk {
                            request_id: request_id.to_string(),
                            channel: StreamChannel::ToolCall,
                            content: args_str.clone(),
                            tool_call: Some(ToolCallChunk {
                                tool_call_id: func_call.id.clone().unwrap_or_else(|| {
                                    uuid::Uuid::new_v4().to_string()
                                }),
                                tool_name: func_call.name.clone(),
                                arguments: args_str,
                            }),
                            usage: usage.clone(),
                        }));
                    }
                }
            }
        }

        results
    }
}

// ---- Gemini API 数据结构 ----

/// Gemini 流式 chunk
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiStreamChunk {
    candidates: Vec<GeminiCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    #[serde(default, rename = "finishReason")]
    finish_reason: Option<String>,
    #[serde(default, rename = "safetyRatings")]
    safety_ratings: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiContent {
    #[serde(default)]
    parts: Vec<GeminiPart>,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiPart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thought: Option<String>,
    #[serde(default, rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(default, rename = "functionResponse")]
    function_response: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiFunctionCall {
    #[serde(default)]
    id: Option<String>,
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiUsageMetadata {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(rename = "totalTokenCount")]
    total_token_count: u32,
}

/// Gemini 非流式响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

#[async_trait::async_trait]
impl Provider for GeminiProvider {
    async fn stream(
        &self,
        request: SampleRequest,
        cancel: CancellationHandle,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk>> + Send>>> {
        let body = self.build_request_body(&request);
        let request_id = request.request_id.clone();
        let model = request.model.clone();

        tracing::debug!(
            target: "ccore::sampler",
            provider = %self.config.name,
            model = %model,
            "gemini sampling request"
        );

        let response = self
            .client
            .post(self.stream_url(&model))
            .header("x-goog-api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .headers(
                request.extra_headers.iter().fold(
                    reqwest::header::HeaderMap::new(),
                    |mut map, (k, v)| {
                        if let (Ok(name), Ok(value)) = (
                            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                            reqwest::header::HeaderValue::from_str(v),
                        ) {
                            map.insert(name, value);
                        }
                        map
                    },
                ),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Gemini HTTP 请求失败：{}", e))?;

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
                error = %format!("Gemini API error {}: {}", status, body_text.chars().take(200).collect::<String>()),
                "sampling request failed"
            );
            return Err(anyhow!("Gemini API 返回错误 {}：{}", status, body_text));
        }

        let byte_stream = response.bytes_stream();

        let rid = request_id.clone();
        let stream = byte_stream
            .scan(SseParserState::default(), move |state, chunk_result| {
                if cancel.is_cancelled() {
                    return std::future::ready(None);
                }

                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        return std::future::ready(Some(vec![Err(anyhow!(
                            "Gemini 流读取错误：{}",
                            e
                        ))]));
                    }
                };

                state.buffer.push_str(&String::from_utf8_lossy(&chunk));

                let mut results = Vec::new();

                while let Some(pos) = state.buffer.find('\n') {
                    let line = state.buffer[..pos].trim().to_string();
                    state.buffer.drain(..=pos);

                    if let Some(stripped) = line.strip_prefix("data: ") {
                        if !state.current_data.is_empty() {
                            state.current_data.push('\n');
                        }
                        state.current_data.push_str(stripped);
                    } else if line.is_empty() {
                        if !state.current_data.is_empty() {
                            let chunks = Self::parse_sse_data(&rid, &state.current_data);
                            results.extend(chunks);
                        }
                        state.current_data.clear();
                    }
                }

                std::future::ready(Some(results))
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }

    async fn sample(&self, request: SampleRequest) -> Result<SampleResponse> {
        let body = self.build_request_body(&request);
        let model = request.model.clone();

        let response = self
            .client
            .post(self.generate_url(&model))
            .header("x-goog-api-key", &self.config.api_key)
            .header("Content-Type", "application/json")
            .headers(
                request.extra_headers.iter().fold(
                    reqwest::header::HeaderMap::new(),
                    |mut map, (k, v)| {
                        if let (Ok(name), Ok(value)) = (
                            reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                            reqwest::header::HeaderValue::from_str(v),
                        ) {
                            map.insert(name, value);
                        }
                        map
                    },
                ),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Gemini HTTP 请求失败：{}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow!("Gemini API 返回错误 {}：{}", status, body_text));
        }

        let resp: GeminiResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Gemini 响应 JSON 解析失败：{}", e))?;

        let usage = resp.usage_metadata.unwrap_or(GeminiUsageMetadata {
            prompt_token_count: 0,
            candidates_token_count: 0,
            total_token_count: 0,
        });

        Ok(SampleResponse {
            request_id: request.request_id,
            usage: TokenUsage {
                prompt_tokens: usage.prompt_token_count,
                completion_tokens: usage.candidates_token_count,
                total_tokens: usage.total_token_count,
            },
            finish_reason: resp
                .candidates
                .first()
                .and_then(|c| c.finish_reason.clone())
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

/// SSE 解析器状态
#[derive(Default)]
struct SseParserState {
    buffer: String,
    current_data: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_text_chunk() {
        let data = r#"{"candidates": [{"content": {"parts": [{"text": "Hello"}], "role": "model"}, "finishReason": "STOP"}], "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 5, "totalTokenCount": 15}}"#;
        let chunks = GeminiProvider::parse_sse_data("req-1", data);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::Text));
        assert_eq!(chunk.content, "Hello");
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
    }

    #[test]
    fn test_parse_thought_chunk() {
        let data = r#"{"candidates": [{"content": {"parts": [{"thought": "Let me think about this..."}], "role": "model"}}]}"#;
        let chunks = GeminiProvider::parse_sse_data("req-1", data);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::Reasoning));
        assert_eq!(chunk.content, "Let me think about this...");
    }

    #[test]
    fn test_parse_function_call() {
        let data = r#"{"candidates": [{"content": {"parts": [{"functionCall": {"name": "bash", "args": {"command": "ls"}}}], "role": "model"}}]}"#;
        let chunks = GeminiProvider::parse_sse_data("req-1", data);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::ToolCall));
        let tc = chunk.tool_call.as_ref().unwrap();
        assert_eq!(tc.tool_name, "bash");
    }

    #[test]
    fn test_build_request_body() {
        let config = GeminiConfig {
            name: "gemini".into(),
            api_key: "test-key".into(),
            base_url: "https://generativelanguage.googleapis.com".into(),
            models: vec!["gemini-2.5-pro".into()],
        };
        let provider = GeminiProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "gemini-2.5-pro".into(),
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
            top_p: None,
            thinking: None,
            system_prompt: Some("You are a helpful assistant.".into()),
            tool_choice: None,
            prompt_cache_key: None,
            goal_verify: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let body = provider.build_request_body(&request);
        // 系统提示应在 systemInstruction 中
        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "You are a helpful assistant."
        );
        // 消息角色应为 user（非 assistant 不变）
        assert_eq!(body["contents"][0]["role"], "user");
    }

    #[test]
    fn test_build_request_body_with_tools() {
        let config = GeminiConfig {
            name: "gemini".into(),
            api_key: "test-key".into(),
            base_url: "https://generativelanguage.googleapis.com".into(),
            models: vec!["gemini-2.5-pro".into()],
        };
        let provider = GeminiProvider::new(config);

        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "gemini-2.5-pro".into(),
            messages: vec![super::super::provider::ChatMessage {
                role: "user".into(),
                content: "List files".into(),
                cache_control: None,
            }],
            tools: vec![super::super::provider::ToolDefinition {
                name: "bash".into(),
                description: "Execute bash".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            stream: true,
            reasoning_effort: None,
            max_tokens: Some(4096),
            temperature: Some(0.5),
            top_p: None,
            thinking: None,
            system_prompt: None,
            tool_choice: Some(super::super::provider::ToolChoice::Auto),
            prompt_cache_key: None,
            goal_verify: false,
            extra_headers: std::collections::HashMap::new(),
        };

        let body = provider.build_request_body(&request);
        // 工具定义应在 functionDeclarations 中
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert!(tools[0].get("functionDeclarations").is_some());
        // 工具选择
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        // max_tokens → maxOutputTokens
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 4096);
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
    }
}