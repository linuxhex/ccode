//! Sampler Node - LLM 采样请求处理
//!
//! Fusion: 生产 SamplerActor 能力并入本 Node。
//! 总线契约不变：sub sampler/request + sampler/*/cancel；pub sampler/{id}/stream
//!
//! Sampler Node 的职责：
//! 1. 接收 Agent 的 sampler/request 消息
//! 2. 根据 model 字段路由到对应 Provider
//! 3. 调用 Provider 的 stream 方法获取 LLM 响应
//! 4. 将每个 StreamChunk 封装为消息，发布到 sampler/{req_id}/stream topic
//! 5. 流式结束后发送 SampleResponse（含 token usage）
//! 6. 支持取消信号（sampler/{req_id}/cancel）
//! 7. 发射 SamplerEvent 供外部消费
//! 8. 生产级重试/ fallback provider（超时、可重试 HTTP 错误、provider fallback）

use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use tokio::time::sleep;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::sampler::provider::{SampleRequest, StreamChunk, StreamChannel, TokenUsage, CancellationHandle, SamplerEvent};
use crate::sampler::router::ProviderRouter;
use crate::sampler::retry::{classify_sampler_error, resolve_max_retries, RetryDecision};
use crate::config::provider::ProviderConfig;

/// Sampler Node 实现
pub struct SamplerNode {
    id: NodeId,
    /// 多模型路由器
    router: ProviderRouter,
    /// 最大重试次数（从环境变量或默认值解析）
    max_retries: u32,
    /// 活跃请求的取消句柄（request_id → CancellationHandle）
    cancel_handles: HashMap<String, CancellationHandle>,
}

impl SamplerNode {
    /// 创建 SamplerNode，使用空的 ProviderRouter
    pub fn new(id: NodeId) -> Self {
        let router = ProviderRouter::from_configs(&[]);
        let max_retries = resolve_max_retries(None);
        Self { id, router, max_retries, cancel_handles: HashMap::new() }
    }

    /// 使用配置列表创建 SamplerNode
    pub fn with_configs(id: NodeId, configs: &[ProviderConfig]) -> Self {
        let router = ProviderRouter::from_configs(configs);
        let max_retries = resolve_max_retries(None);
        Self { id, router, max_retries, cancel_handles: HashMap::new() }
    }

    /// 尝试从指定 Provider 获取流式响应，失败时自动 fallback
    ///
    /// 返回 (stream, provider_name)
    async fn try_stream_with_fallback(
        &mut self,
        request: &SampleRequest,
        cancel: CancellationHandle,
    ) -> anyhow::Result<(std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamChunk>> + Send>>, String)> {
        // 第一次尝试：主 Provider
        let model = &request.model;
        let mut failed_provider_name: Option<String> = None;
        if let Some(provider_name) = self.router.find_provider_name(model) {
            let name = provider_name.clone();
            let provider = self.router.get_provider_mut(&provider_name)
                .ok_or_else(|| anyhow::anyhow!("Provider {} 不存在", provider_name))?;
            match provider.stream(request.clone(), cancel.clone()).await {
                Ok(stream) => return Ok((stream, name)),
                Err(e) => {
                    tracing::warn!("Provider {} 采样失败：{}, 尝试 fallback", name, e);
                    failed_provider_name = Some(name);
                }
            }
        }

        // 第二次尝试：fallback Provider（排除已失败的 provider）
        if let Some(ref name) = failed_provider_name {
            if let Some(provider_name) = self.router.find_fallback_name(name, model) {
                let fallback_name = provider_name.clone();
                tracing::info!("Fallback 到 Provider：{}", fallback_name);
                if let Some(provider) = self.router.get_provider_mut(&provider_name) {
                    match provider.stream(request.clone(), cancel.clone()).await {
                        Ok(stream) => return Ok((stream, fallback_name)),
                        Err(e) => {
                            tracing::warn!("Fallback Provider {} 也失败：{}", fallback_name, e);
                        }
                    }
                }
            }
        }

        Err(anyhow::anyhow!("未找到模型 {} 的可用 Provider", model))
    }

    /// 将流式 chunk 通过消息总线逐个发送（优先走数据面 PUB）
    ///
    /// `collect_text` 为 true 时，额外累积 Text 通道内容并随返回值传出
    /// （供 GoalLoop 验证请求解析 LLM 的 JSON 响应）。
    async fn stream_to_bus(
        &mut self,
        request_id: &str,
        model: &str,
        mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamChunk>> + Send>>,
        transport: &NodeTransportHandle,
        collect_text: bool,
    ) -> anyhow::Result<(TokenUsage, String)> {
        tracing::debug!("开始采样：model={}, request_id={}", model, request_id);

        // 累积 token 使用量
        let mut accumulated_usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        // 累积文本内容（仅 collect_text 时使用）
        let mut text_acc = String::new();

        // 逐 chunk 读取流式响应
        while let Some(chunk_result) = stream.next().await {
            // 检查取消信号
            if let Some(handle) = self.cancel_handles.get(request_id) {
                if handle.is_cancelled() {
                    // 发射取消事件
                    let cancel_event = SamplerEvent::Cancelled {
                        request_id: request_id.to_string(),
                    };
                    let _ = self.emit_event(&cancel_event, transport).await;
                    tracing::info!("采样已取消：request_id={}", request_id);
                    break;
                }
            }

            match chunk_result {
                Ok(chunk) => {
                    // 累积 usage
                    if let Some(ref usage) = chunk.usage {
                        // Anthropic 流式 API：message_start 报告 input_tokens，message_delta 报告累计 output_tokens
                        // 两者都是最终值（非增量），取 max 确保捕获最终统计
                        accumulated_usage.prompt_tokens = accumulated_usage.prompt_tokens.max(usage.prompt_tokens);
                        if usage.completion_tokens > 0 {
                            accumulated_usage.completion_tokens = accumulated_usage.completion_tokens.max(usage.completion_tokens);
                        }
                        accumulated_usage.total_tokens = accumulated_usage.prompt_tokens + accumulated_usage.completion_tokens;
                    }

                    // 累积文本内容（GoalLoop 验证请求需要完整响应解析 JSON）
                    if collect_text && matches!(chunk.channel, StreamChannel::Text) {
                        text_acc.push_str(&chunk.content);
                    }

                    let stream_topic = Topic::sampler_stream(&chunk.request_id);
                    let chunk_msg = FrameCodec::new_message(
                        stream_topic,
                        self.id.as_str(),
                        &chunk,
                    )?;
                    // 优先数据面 PUB，回退控制面
                    if let Err(_) = transport.publish_data(&chunk_msg).await {
                        transport.send_message(&FrameCodec::new_message(
                            Topic::sampler_stream(&chunk.request_id),
                            self.id.as_str(),
                            &chunk,
                        )?).await?;
                    }
                }
                Err(e) => {
                    tracing::warn!("Stream chunk 错误：{}", e);
                    AgentMetrics::global().record_error("sampler_stream_chunk_error");

                    // 发射错误事件
                    let error_event = SamplerEvent::Error {
                        request_id: request_id.to_string(),
                        error: format!("{}", e),
                    };
                    let _ = self.emit_event(&error_event, transport).await;

                    let err_msg = FrameCodec::new_message(
                        Topic::sampler_stream(request_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "error": format!("{}", e),
                            "request_id": request_id,
                        }),
                    )?;
                    if let Err(_) = transport.publish_data(&err_msg).await {
                        transport.send_message(&err_msg).await?;
                    }
                    break;
                }
            }
        }

        // 流式结束，发送 done 消息（使用累积的 usage）
        let done_msg = FrameCodec::new_message(
            Topic::sampler_stream(request_id),
            self.id.as_str(),
            &serde_json::json!({
                "type": "done",
                "request_id": request_id,
                "usage": {
                    "prompt_tokens": accumulated_usage.prompt_tokens,
                    "completion_tokens": accumulated_usage.completion_tokens,
                    "total_tokens": accumulated_usage.total_tokens,
                },
                "finish_reason": "stop",
            }),
        )?;
        if let Err(_) = transport.publish_data(&done_msg).await {
            transport.send_message(&done_msg).await?;
        }

        // 发射完成事件
        let completed_event = SamplerEvent::Completed {
            request_id: request_id.to_string(),
            usage: accumulated_usage.clone(),
            finish_reason: "stop".to_string(),
        };
        let _ = self.emit_event(&completed_event, transport).await;

        // 清理取消句柄
        self.cancel_handles.remove(request_id);

        tracing::debug!("采样完成：model={}, request_id={}", model, request_id);
        Ok((accumulated_usage, text_acc))
    }

    /// 发射 SamplerEvent 到消息总线
    async fn emit_event(
        &self,
        event: &SamplerEvent,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let request_id = match event {
            SamplerEvent::Started { request_id, .. } => request_id,
            SamplerEvent::TextDelta { request_id, .. } => request_id,
            SamplerEvent::ThinkingDelta { request_id, .. } => request_id,
            SamplerEvent::ToolCallStart { request_id, .. } => request_id,
            SamplerEvent::ToolCallDelta { request_id, .. } => request_id,
            SamplerEvent::ToolCallEnd { request_id, .. } => request_id,
            SamplerEvent::Completed { request_id, .. } => request_id,
            SamplerEvent::Error { request_id, .. } => request_id,
            SamplerEvent::Cancelled { request_id, .. } => request_id,
        };
        let event_topic = Topic::new(format!("sampler/{}/stream/event", request_id));
        let event_msg = FrameCodec::new_message(
            event_topic,
            self.id.as_str(),
            event,
        )?;
        if let Err(_) = transport.publish_data(&event_msg).await {
            transport.send_message(&event_msg).await?;
        }
        Ok(())
    }

    /// 取消进行中的采样请求
    ///
    /// 1. 设置取消标志（stream loop 会检查）
    /// 2. 向 stream topic 发送最终 cancelled 事件
    async fn cancel_request(
        &mut self,
        request_id: &str,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        if let Some(handle) = self.cancel_handles.get(request_id) {
            handle.cancel();
            tracing::info!("收到取消信号：request_id={}", request_id);

            // 发送最终 cancelled 消息到 stream
            let cancelled_msg = FrameCodec::new_message(
                Topic::sampler_stream(request_id),
                self.id.as_str(),
                &serde_json::json!({
                    "type": "cancelled",
                    "request_id": request_id,
                }),
            )?;
            if let Err(e) = transport.publish_data(&cancelled_msg).await {
                tracing::warn!("data-plane publish failed: {}, falling back", e);
                transport.send_message(&cancelled_msg).await?;
            }

            // 发射取消事件
            let cancel_event = SamplerEvent::Cancelled {
                request_id: request_id.to_string(),
            };
            let _ = self.emit_event(&cancel_event, transport).await;

            // 清理取消句柄
            self.cancel_handles.remove(request_id);
        } else {
            tracing::warn!("取消请求不存在或已完成：request_id={}", request_id);
        }
        Ok(())
    }

    /// 解析 GoalLoop 验证 LLM 响应并发送结果到 cortex/goal_verify_result
    ///
    /// LLM 应返回 JSON：{"passed": true/false, "reasoning": "..."}。
    /// 解析容错：尝试直接解析、提取 {...} 子串、关键词兜底。
    async fn send_goal_verify_result(
        &self,
        request_id: &str,
        text: &str,
        transport: &NodeTransportHandle,
    ) {
        let passed = Self::parse_goal_verify_json(text);

        let result_json = serde_json::json!({
            "passed": passed,
            "reasoning": text.chars().take(200).collect::<String>(),
        });
        tracing::info!(
            target: "ccore::goal",
            request_id,
            passed,
            "GoalLoop LLM 验证完成，结果发往 cortex/goal_verify_result"
        );
        if let Ok(result_msg) = FrameCodec::new_message(
            Topic::new("cortex/goal_verify_result"),
            self.id.as_str(),
            &result_json,
        ) {
            if let Err(e) = transport.publish_data(&result_msg).await {
                tracing::debug!("数据面 PUB 发送 goal_verify_result 失败，回退控制面：{}", e);
                if let Err(e) = transport.send_message(&result_msg).await {
                    tracing::warn!("发送 GoalLoop 验证结果失败：{}", e);
                }
            }
        }
    }

    /// 从 LLM 文本响应中解析验证结果（passed）
    fn parse_goal_verify_json(text: &str) -> bool {
        // 1. 直接解析整个文本为 JSON
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            if let Some(p) = v.get("passed").and_then(|x| x.as_bool()) {
                return p;
            }
        }
        // 2. 提取第一个 {...} 子串解析
        if let Some(start) = text.find('{') {
            if let Some(end) = text.rfind('}') {
                if end > start {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) {
                        if let Some(p) = v.get("passed").and_then(|x| x.as_bool()) {
                            return p;
                        }
                    }
                }
            }
        }
        // 3. 关键词兜底
        let lower = text.to_lowercase();
        if lower.contains("\"passed\": true") || lower.contains("已通过") || lower.contains("验证通过") {
            return true;
        }
        if lower.contains("\"passed\": false") || lower.contains("未通过") || lower.contains("验证失败") {
            return false;
        }
        // 无法解析时默认不通过（保守判定，触发重试）
        false
    }

    /// 处理采样请求（带重试逻辑）
    async fn handle_sample_request(
        &mut self,
        request: SampleRequest,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let request_id = request.request_id.clone();
        let model = request.model.clone();
        // 记录采样起始时间，用于计算推理延迟（从请求到响应的耗时）
        let start = std::time::Instant::now();

        // 创建取消句柄并注册
        let cancel_handle = CancellationHandle::new();
        self.cancel_handles.insert(request_id.clone(), cancel_handle.clone());

        // 发射开始事件
        let started_event = SamplerEvent::Started {
            request_id: request_id.clone(),
            model: model.clone(),
        };
        let _ = self.emit_event(&started_event, transport).await;

        let mut retry_count: u32 = 0;

        loop {
            match self.try_stream_with_fallback(&request, cancel_handle.clone()).await {
                Ok((stream, provider_name)) => {
                    tracing::info!("采样开始：model={}, provider={}", model, provider_name);
                    let result = self
                        .stream_to_bus(&request_id, &model, stream, transport, request.goal_verify)
                        .await;
                    // 采样完成，记录推理延迟（从请求到响应的耗时）
                    AgentMetrics::global()
                        .record_inference_latency(start.elapsed().as_millis() as f64);
                    // GoalLoop 验证闭环：解析 LLM 的 JSON 响应，结果发到 cortex/goal_verify_result
                    if request.goal_verify {
                        if let Ok((_, text)) = &result {
                            self.send_goal_verify_result(&request_id, text, transport).await;
                        }
                    }
                    return result.map(|_| ());
                }
                Err(e) => {
                    let error_message = format!("{}", e);
                    // 尝试从错误中提取 HTTP 状态码
                    let status_code = extract_status_code(&error_message);

                    let decision = classify_sampler_error(
                        &error_message,
                        status_code,
                        retry_count,
                        self.max_retries,
                    );

                    match decision {
                        RetryDecision::Retry { backoff } => {
                            retry_count += 1;
                            tracing::warn!(
                                "采样失败（第 {} 次重试），等待 {:?} 后重试：{}",
                                retry_count, backoff, error_message
                            );
                            sleep(backoff).await;
                        }
                        RetryDecision::RetryWithBackoff { backoff, is_rate_limited: _ } => {
                            retry_count += 1;
                            tracing::warn!(
                                "速率限制（第 {} 次重试），等待 {:?} 后重试：{}",
                                retry_count, backoff, error_message
                            );
                            sleep(backoff).await;
                        }
                        RetryDecision::RetryImmediate { backoff } => {
                            retry_count += 1;
                            tracing::warn!(
                                "Doom Loop 检测（第 {} 次重试），近立即重试：{}",
                                retry_count, error_message
                            );
                            sleep(backoff).await;
                        }
                        RetryDecision::Fatal(reason) => {
                            tracing::error!("采样致命错误：{}", reason);
                            AgentMetrics::global().record_error("sampler_fatal");

                            // 发射错误事件
                            let error_event = SamplerEvent::Error {
                                request_id: request_id.clone(),
                                error: reason.clone(),
                            };
                            let _ = self.emit_event(&error_event, transport).await;

                            let err_msg = FrameCodec::new_message(
                                Topic::sampler_stream(&request_id),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "error": format!("{}", e),
                                    "request_id": request_id,
                                    "retry_count": retry_count,
                                }),
                            )?;
                            transport.send_message(&err_msg).await?;
                            // 清理取消句柄
                            self.cancel_handles.remove(&request_id);
                            return Err(e);
                        }
                    }
                }
            }
        }
    }
}

/// 从错误消息中尝试提取 HTTP 状态码
fn extract_status_code(error_message: &str) -> Option<u16> {
    // 尝试匹配常见的 HTTP 状态码模式，如 "HTTP 429"、"status 500" 等
    let msg = error_message.to_lowercase();
    for prefix in &["http ", "status "] {
        if let Some(pos) = msg.find(prefix) {
            let after = &msg[pos + prefix.len()..];
            if let Some(code) = after.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u16>().ok() {
                if code >= 400 && code < 600 {
                    return Some(code);
                }
            }
        }
    }
    None
}

#[async_trait]
impl Node for SamplerNode {
    fn node_type(&self) -> NodeType {
        NodeType::Sampler
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Sampler Node 启动：{} (providers={}, models={:?})",
            self.id,
            self.router.provider_count(),
            self.router.available_models(),
        );
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();
        if topic == "sampler/request" {
            let request: SampleRequest = FrameCodec::decode_payload(&msg)?;
            self.handle_sample_request(request, transport).await?;
        } else if topic.starts_with("sampler/") && topic.ends_with("/cancel") {
            // 提取 request_id：sampler/{req_id}/cancel → req_id
            let trimmed = topic.strip_prefix("sampler/").unwrap_or("");
            let req_id = trimmed.strip_suffix("/cancel").unwrap_or("");
            if !req_id.is_empty() {
                self.cancel_request(req_id, transport).await?;
            }
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "sampler/request".into(),
            "sampler/*/cancel".into(),
        ]
    }

    /// Sampler 发布的 topic（数据面 PUB）
    fn published_topics(&self) -> Vec<String> {
        vec!["sampler/*/stream".into()]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Sampler Node 关闭：{}", self.id);
        Ok(())
    }
}
