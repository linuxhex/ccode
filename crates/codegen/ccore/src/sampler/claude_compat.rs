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
    ChatMessage, Provider, SampleRequest, SampleResponse, StreamChannel, StreamChunk, TokenUsage,
    ToolCallChunk, CancellationHandle,
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
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(300))
            .pool_idle_timeout(std::time::Duration::from_secs(600))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { config, client }
    }

    /// 构造 Messages API 请求 URL
    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// 从消息列表中提取 system 角色条目
    ///
    /// `messages` 数组里 Anthropic 只接受 user/assistant，出现 `role: "system"`
    /// 会被拒绝。但 ccore 的工作记忆会在对话中途注入 system 条目
    /// （反射弧建议、doom loop 逃脱提示、元认知建议、压缩摘要、CCODE.md 项目上下文），
    /// 且 `build_sample_request` 把 `system_prompt` 置为 None，全都落在 messages 里。
    ///
    /// 返回 (合并后的 system 文本, 剔除 system 后的消息列表)。
    /// system 文本按出现顺序拼接，交由调用方放到顶层 `system` 字段。
    pub(crate) fn extract_system_messages(
        messages: &[ChatMessage],
    ) -> (Option<String>, Vec<ChatMessage>) {
        let mut system_parts: Vec<&str> = Vec::new();
        let mut rest: Vec<ChatMessage> = Vec::with_capacity(messages.len());

        for m in messages {
            if m.role == "system" {
                if !m.content.trim().is_empty() {
                    system_parts.push(m.content.as_str());
                }
            } else {
                rest.push(m.clone());
            }
        }

        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (system, rest)
    }

    /// 合并连续同角色消息
    ///
    /// Anthropic Messages API 要求 user/assistant 严格交替，连续同角色会被拒绝。
    /// 而 ccore 的工作记忆里，N 个并行工具的结果是 N 条独立 user 消息
    /// （`WorkingEntry` 无 tool_call_id、`MessageRole` 无 Tool 变体，暂无法表达
    /// 单条 user 消息内含多个 tool_result 块的标准形状），因此在序列化边界合并。
    ///
    /// 合并后保留首条的 `cache_control`——断点应落在稳定前缀的起始处。
    /// OpenAI 兼容端点容忍连续同角色，故本处理只在 Anthropic provider 做。
    pub(crate) fn merge_consecutive_roles(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        let mut merged: Vec<ChatMessage> = Vec::with_capacity(messages.len());
        for m in messages {
            match merged.last_mut() {
                Some(prev) if prev.role == m.role => {
                    prev.content.push_str("\n\n");
                    prev.content.push_str(&m.content);
                    // 已有断点则保留，否则继承当前条的
                    if prev.cache_control.is_none() {
                        prev.cache_control = m.cache_control.clone();
                    }
                }
                _ => merged.push(m.clone()),
            }
        }
        merged
    }

    /// 将 SampleRequest 转换为 Anthropic Messages API 请求体
    fn build_request_body(&self, request: &SampleRequest) -> serde_json::Value {
        // 先剔除 system 条目（Anthropic 的 messages 只接受 user/assistant），
        // 再合并连续同角色（要求严格交替）——顺序不能反：先剔除才能让原本被
        // system 条目隔开的两条同角色消息正确合并。
        let (inline_system, without_system) = Self::extract_system_messages(&request.messages);
        let merged_messages = Self::merge_consecutive_roles(&without_system);
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": merged_messages.iter().map(|m| {
                let mut msg = serde_json::json!({
                    "role": m.role,
                    "content": m.content,
                });
                // 序列化 cache_control 断点，让 API 侧复用前缀 KV cache
                if let Some(ref cc) = m.cache_control {
                    msg.as_object_mut().unwrap().insert(
                        "cache_control".to_string(),
                        serde_json::json!({"type": cc.cache_type}),
                    );
                }
                msg
            }).collect::<Vec<_>>(),
            "stream": request.stream,
            // Anthropic 要求必须设置 max_tokens
            "max_tokens": request.max_tokens.unwrap_or(8192),
        });

        // 系统提示作为顶层 system 字段（而非放在 messages 中）。
        // request.system_prompt 与从 messages 里剔出的 inline system 条目都要带上：
        // 前者是显式设置的，后者是工作记忆中途注入的（反射弧/doom loop 提示等），
        // 两者都丢不得，按 显式 → 内联 的顺序拼接。
        let system_text = match (request.system_prompt.as_deref(), inline_system.as_deref()) {
            (Some(explicit), Some(inline)) => Some(format!("{explicit}\n\n{inline}")),
            (Some(explicit), None) => Some(explicit.to_string()),
            (None, Some(inline)) => Some(inline.to_string()),
            (None, None) => None,
        };
        if let Some(ref system_prompt) = system_text {
            body["system"] = serde_json::json!(system_prompt);
        }

        // 工具定义（按名字排序确保跨轮次稳定，提升缓存命中率）
        if !request.tools.is_empty() {
            let mut sorted_tools: Vec<_> = request.tools.iter().collect();
            sorted_tools.sort_by(|a, b| a.name.cmp(&b.name));

            let mut tools_json: Vec<serde_json::Value> = sorted_tools.iter().map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            }).collect();

            // 最后一个工具加 cache_control 断点，缓存整个工具定义前缀
            if let Some(last) = tools_json.last_mut() {
                last.as_object_mut().unwrap().insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
            }
            body["tools"] = serde_json::json!(tools_json);

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

        // top_p 核采样
        if let Some(top_p) = request.top_p {
            body["top_p"] = serde_json::json!(top_p);
        }

        // Claude extended thinking 配置
        // Anthropic 格式：{"thinking": {"type": "enabled", "budget_tokens": 4096}}
        if let Some(ref thinking) = request.thinking {
            if thinking.enabled {
                let thinking_config = serde_json::json!({
                    "type": "enabled",
                    "budget_tokens": thinking.budget_tokens.unwrap_or(4096),
                });
                body["thinking"] = thinking_config;
                // Claude 要求启用 thinking 时 temperature 必须为 1
                if request.temperature.is_none() {
                    body["temperature"] = serde_json::json!(1.0);
                }
            }
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
        // 按 content block index 键控，支持并行 tool_call
        active_tools: &mut std::collections::HashMap<usize, ActiveToolCall>,
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
                        if let Some(tool) = active_tools.get(&delta.index) {
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
                    // 记录当前活跃的工具调用（按 content block index 键控）
                    active_tools.insert(block_start.index, ActiveToolCall {
                        tool_call_id: block_start.content_block.id,
                        tool_name: block_start.content_block.name,
                    });
                } else if block_start.content_block.r#type == "thinking" {
                    // Extended thinking 块开始 - 不需要特殊处理，后续 thinking_delta 会发送内容
                }
            }
            "content_block_stop" => {
                // 内容块结束 - 从活跃工具调用表中移除对应 index
                let block_stop: ClaudeContentBlockStop = match serde_json::from_str(data) {
                    Ok(s) => s,
                    Err(e) => {
                        results.push(Err(anyhow!("content_block_stop JSON 解析失败：{}", e)));
                        return results;
                    }
                };
                active_tools.remove(&block_stop.index);
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
    index: usize,
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
    index: usize,
    content_block: ClaudeContentBlock,
}

/// content_block_stop 事件
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ClaudeContentBlockStop {
    index: usize,
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
                            // 多行 data 追加而非覆盖（SSE 规范允许 data 跨多行，用 \n 连接）
                            if !state.current_data.is_empty() {
                                state.current_data.push('\n');
                            }
                            state.current_data.push_str(stripped);
                        } else if line.is_empty() {
                            // 空行表示事件结束，此时解析
                            if !state.current_event.is_empty() && !state.current_data.is_empty() {
                                let chunks = Self::parse_sse_event(
                                    &rid,
                                    &state.current_event,
                                    &state.current_data,
                                    &mut state.active_tools,
                                );
                                results.extend(chunks);
                            }
                            state.current_event.clear();
                            state.current_data.clear();
                        }
                    }

                    std::future::ready(Some(results))
                },
            )
            .flat_map(futures::stream::iter);

        let stream = super::idle_timeout::with_idle_timeout(
            stream,
            request.idle_timeout_secs,
            request_id,
        );

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

    async fn warm_up(&self) -> Result<()> {
        let url = self.messages_url();
        tracing::info!("预热 Provider {} 连接：{}", self.config.name, url);
        match self.client.head(&url).send().await {
            Ok(resp) => {
                tracing::info!("Provider {} 连接预热完成，status={}", self.config.name, resp.status());
            }
            Err(e) => {
                tracing::warn!("Provider {} 连接预热失败（非致命）：{}", self.config.name, e);
            }
        }
        Ok(())
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
    /// 当前活跃的工具调用（按 content block index 键控，支持并行 tool_call）
    active_tools: std::collections::HashMap<usize, ActiveToolCall>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::provider::CacheControl;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: content.into(),
            cache_control: None,
        }
    }

    /// Anthropic 要求 user/assistant 严格交替。ccore 的工作记忆里 N 个并行工具
    /// 的结果是 N 条独立 user 消息，必须在序列化边界合并否则请求被拒。
    #[test]
    fn merges_consecutive_user_messages() {
        let input = vec![
            msg("user", "帮我看下代码"),
            msg("assistant", "[调用工具：read_file, grep]"),
            msg("user", "文件内容 A"),
            msg("user", "搜索结果 B"),
            msg("user", "搜索结果 C"),
        ];
        let out = ClaudeCompatProvider::merge_consecutive_roles(&input);

        assert_eq!(out.len(), 3, "3 条连续 user 应合并为 1 条");
        assert_eq!(out[0].role, "user");
        assert_eq!(out[1].role, "assistant");
        assert_eq!(out[2].role, "user");
        assert!(out[2].content.contains("文件内容 A"));
        assert!(out[2].content.contains("搜索结果 B"));
        assert!(out[2].content.contains("搜索结果 C"));
    }

    /// 合并后必须严格交替，这是 Anthropic 的硬性要求。
    #[test]
    fn merged_messages_strictly_alternate() {
        let input = vec![
            msg("user", "a"),
            msg("user", "b"),
            msg("assistant", "c"),
            msg("assistant", "d"),
            msg("user", "e"),
        ];
        let out = ClaudeCompatProvider::merge_consecutive_roles(&input);

        for pair in out.windows(2) {
            assert_ne!(
                pair[0].role, pair[1].role,
                "合并后不应存在连续同角色：{:?}",
                out.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
            );
        }
        assert_eq!(out.len(), 3);
    }

    /// 合并时 cache_control 断点不能丢——断点落在稳定前缀起始处。
    #[test]
    fn merge_preserves_cache_control() {
        let mut first = msg("user", "第一条");
        first.cache_control = Some(CacheControl::ephemeral());
        let input = vec![first, msg("user", "第二条")];

        let out = ClaudeCompatProvider::merge_consecutive_roles(&input);
        assert_eq!(out.len(), 1);
        assert!(out[0].cache_control.is_some(), "首条的断点应保留");
    }

    /// 首条无断点、后续条有断点时，应继承过来而非丢弃。
    #[test]
    fn merge_inherits_cache_control_from_later_message() {
        let mut second = msg("user", "第二条");
        second.cache_control = Some(CacheControl::ephemeral());
        let input = vec![msg("user", "第一条"), second];

        let out = ClaudeCompatProvider::merge_consecutive_roles(&input);
        assert_eq!(out.len(), 1);
        assert!(out[0].cache_control.is_some(), "应继承后续条的断点");
    }

    #[test]
    fn merge_leaves_already_alternating_untouched() {
        let input = vec![msg("user", "a"), msg("assistant", "b"), msg("user", "c")];
        let out = ClaudeCompatProvider::merge_consecutive_roles(&input);
        assert_eq!(out.len(), 3, "已交替的序列不应被改动");
        assert_eq!(out[0].content, "a");
        assert_eq!(out[2].content, "c");
    }

    #[test]
    fn merge_handles_empty_input() {
        let out = ClaudeCompatProvider::merge_consecutive_roles(&[]);
        assert!(out.is_empty());
    }

    /// Anthropic 的 messages 只接受 user/assistant。ccore 工作记忆会在对话中途
    /// 注入 system 条目（反射弧建议/doom loop 提示/元认知建议/压缩摘要/CCODE.md），
    /// 必须剔出来放顶层 system 字段，否则请求被拒。
    #[test]
    fn extracts_inline_system_messages() {
        let input = vec![
            msg("system", "你是编码助手"),
            msg("user", "帮我看下代码"),
            msg("assistant", "好的"),
            msg("system", "[反射弧建议] 换个方法"),
            msg("user", "继续"),
        ];
        let (system, rest) = ClaudeCompatProvider::extract_system_messages(&input);

        let system = system.expect("应提取出 system 文本");
        assert!(system.contains("你是编码助手"));
        assert!(system.contains("[反射弧建议] 换个方法"), "中途注入的 system 也要提取");

        assert_eq!(rest.len(), 3);
        assert!(
            rest.iter().all(|m| m.role != "system"),
            "剔除后不应残留 system 角色：{:?}",
            rest.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
        );
    }

    /// 剔除 system 必须在合并之前——否则原本被 system 条目隔开的两条同角色
    /// 消息剔除后变成相邻，却已错过合并时机，仍是连续同角色。
    #[test]
    fn extract_before_merge_yields_alternating_messages() {
        // user → system → user：剔掉 system 后两条 user 相邻，必须再合并
        let input = vec![
            msg("user", "第一个工具结果"),
            msg("system", "[doom loop] 换个方法"),
            msg("user", "第二个工具结果"),
        ];

        let (system, rest) = ClaudeCompatProvider::extract_system_messages(&input);
        assert!(system.is_some());

        let merged = ClaudeCompatProvider::merge_consecutive_roles(&rest);
        assert_eq!(merged.len(), 1, "剔除 system 后两条 user 应合并为一条");
        assert!(merged[0].content.contains("第一个工具结果"));
        assert!(merged[0].content.contains("第二个工具结果"));

        for pair in merged.windows(2) {
            assert_ne!(pair[0].role, pair[1].role);
        }
    }

    #[test]
    fn extract_returns_none_when_no_system_messages() {
        let input = vec![msg("user", "a"), msg("assistant", "b")];
        let (system, rest) = ClaudeCompatProvider::extract_system_messages(&input);
        assert!(system.is_none());
        assert_eq!(rest.len(), 2);
    }

    /// 空白 system 条目不应产生一个只含空串的顶层 system 字段。
    #[test]
    fn extract_skips_blank_system_content() {
        let input = vec![msg("system", "   "), msg("user", "a")];
        let (system, rest) = ClaudeCompatProvider::extract_system_messages(&input);
        assert!(system.is_none(), "纯空白的 system 条目应被忽略");
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn test_parse_text_delta() {
        let mut active_tools = std::collections::HashMap::new();
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tools);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert_eq!(chunk.request_id, "req-1");
        assert!(matches!(chunk.channel, StreamChannel::Text));
        assert_eq!(chunk.content, "Hello");
        assert!(chunk.usage.is_none());
    }

    #[test]
    fn test_parse_tool_use_start() {
        let mut active_tools = std::collections::HashMap::new();
        let data = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"bash"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_start", data, &mut active_tools);
        // content_block_start 不产生 StreamChunk，但应记录活跃工具调用
        assert!(chunks.is_empty());
        assert!(active_tools.len() == 1);
        let tool = active_tools.values().next().unwrap();
        assert_eq!(tool.tool_call_id, "toolu_abc");
        assert_eq!(tool.tool_name, "bash");
    }

    #[test]
    fn test_parse_input_json_delta() {
        let mut active_tools = std::collections::HashMap::new();
        active_tools.insert(1, ActiveToolCall {
            tool_call_id: "toolu_abc".into(),
            tool_name: "bash".into(),
        });
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"command\":\"ls\"}"}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tools);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(matches!(chunk.channel, StreamChannel::ToolCall));
        let tc = chunk.tool_call.as_ref().unwrap();
        assert_eq!(tc.tool_name, "bash");
        assert_eq!(tc.tool_call_id, "toolu_abc");
    }

    #[test]
    fn test_parse_thinking_delta() {
        let mut active_tools = std::collections::HashMap::new();
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "content_block_delta", data, &mut active_tools);
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
            top_p: None,
            thinking: None,
            system_prompt: None,
            tool_choice: None,
            prompt_cache_key: None,
            goal_verify: false,
            summarize: false,
            extra_headers: std::collections::HashMap::new(),
            idle_timeout_secs: None,
        };

        let body = provider.build_request_body(&request);
        assert_eq!(body["model"], "claude-sonnet-4-20250514");
        assert_eq!(body["stream"], true);
        // max_tokens 默认 8192
        assert_eq!(body["max_tokens"], 8192);
        // 无 system prompt 时不应该有 system 字段
        assert!(body.get("system").is_none());
    }

    /// 端到端：工作记忆中途注入的 system 条目不得残留在 messages 里，
    /// 且必须出现在顶层 system 字段。这是 Anthropic 请求合法性的硬要求。
    #[test]
    fn request_body_hoists_inline_system_and_alternates() {
        let config = ClaudeCompatConfig {
            name: "claude".into(),
            api_key: "sk-ant-test".into(),
            base_url: "https://api.anthropic.com".into(),
            models: vec!["claude-sonnet-4-20250514".into()],
            api_version: "2023-06-01".into(),
        };
        let provider = ClaudeCompatProvider::new(config);

        // 模拟真实工作记忆形状：CCODE.md 上下文 + 提问 + 工具调用 +
        // 两个并行工具结果，中间被 doom loop 提示（system）隔开
        let request = SampleRequest {
            request_id: "req-1".into(),
            agent_id: "agent-1".into(),
            model: "claude-sonnet-4-20250514".into(),
            messages: vec![
                msg("system", "项目上下文：这是 Rust 工程"),
                msg("user", "帮我看下代码"),
                msg("assistant", "[调用工具：read_file, grep]"),
                msg("user", "文件内容"),
                msg("system", "[doom loop] 换个方法"),
                msg("user", "搜索结果"),
            ],
            tools: Vec::new(),
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            thinking: None,
            system_prompt: None,
            tool_choice: None,
            prompt_cache_key: None,
            goal_verify: false,
            summarize: false,
            extra_headers: std::collections::HashMap::new(),
            idle_timeout_secs: None,
        };

        let body = provider.build_request_body(&request);

        // 两条 system 内容都应进入顶层 system 字段
        let system = body["system"].as_str().expect("应有顶层 system 字段");
        assert!(system.contains("项目上下文"));
        assert!(system.contains("[doom loop]"), "中途注入的 system 也不能丢");

        let msgs = body["messages"].as_array().expect("应有 messages 数组");

        // messages 里不得残留 system 角色
        for m in msgs {
            let role = m["role"].as_str().unwrap();
            assert_ne!(role, "system", "messages 中不应残留 system 角色");
        }

        // 必须严格交替
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        for pair in roles.windows(2) {
            assert_ne!(pair[0], pair[1], "messages 必须严格交替，实际：{roles:?}");
        }

        // 剔除 system 后相邻的两条 user（文件内容 / 搜索结果）应已合并
        let last = msgs.last().unwrap()["content"].as_str().unwrap();
        assert!(last.contains("文件内容"));
        assert!(last.contains("搜索结果"));
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
            top_p: None,
            thinking: None,
            system_prompt: Some("You are a helpful assistant.".into()),
            tool_choice: Some(super::super::provider::ToolChoice::Auto),
            prompt_cache_key: None,
            goal_verify: false,
            summarize: false,
            extra_headers: std::collections::HashMap::new(),
            idle_timeout_secs: None,
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
        let mut active_tools = std::collections::HashMap::new();
        let data = r#"{"type":"message_start","message":{"id":"msg_abc","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-20250514","stop_reason":null,"usage":{"input_tokens":25,"output_tokens":0}}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "message_start", data, &mut active_tools);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.prompt_tokens, 25);
        assert_eq!(usage.completion_tokens, 0);
    }

    #[test]
    fn test_parse_message_delta_with_usage() {
        let mut active_tools = std::collections::HashMap::new();
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#;
        let chunks = ClaudeCompatProvider::parse_sse_event("req-1", "message_delta", data, &mut active_tools);
        assert_eq!(chunks.len(), 1);
        let chunk = chunks[0].as_ref().unwrap();
        assert!(chunk.usage.is_some());
        let usage = chunk.usage.as_ref().unwrap();
        assert_eq!(usage.completion_tokens, 15);
    }
}
