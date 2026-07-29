//! Sampler Node - LLM 采样请求处理
//!
//! Sampler Node 的职责：
//! 1. 接收 Agent 的 sampler/request 消息
//! 2. 根据 model 字段路由到对应 Provider
//! 3. 调用 Provider 的 stream 方法获取 LLM 响应
//! 4. 将每个 StreamChunk 封装为消息，发布到 sampler/{req_id}/stream topic
//! 5. 流式结束后发送 SampleResponse（含 token usage）

use async_trait::async_trait;
use futures::StreamExt;
use tokio::time::sleep;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::sampler::provider::{SampleRequest, StreamChunk};
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
}

impl SamplerNode {
    /// 创建 SamplerNode，使用空的 ProviderRouter
    pub fn new(id: NodeId) -> Self {
        let router = ProviderRouter::from_configs(&[]);
        let max_retries = resolve_max_retries(None);
        Self { id, router, max_retries }
    }

    /// 使用配置列表创建 SamplerNode
    pub fn with_configs(id: NodeId, configs: &[ProviderConfig]) -> Self {
        let router = ProviderRouter::from_configs(configs);
        let max_retries = resolve_max_retries(None);
        Self { id, router, max_retries }
    }

    /// 尝试从指定 Provider 获取流式响应，失败时自动 fallback
    ///
    /// 返回 (stream, provider_name)
    async fn try_stream_with_fallback(
        &mut self,
        request: &SampleRequest,
    ) -> anyhow::Result<(std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamChunk>> + Send>>, String)> {
        // 第一次尝试：主 Provider
        let model = &request.model;
        let mut failed_provider_name: Option<String> = None;
        if let Some(provider_name) = self.router.find_provider_name(model) {
            let name = provider_name.clone();
            let provider = self.router.get_provider_mut(&provider_name)
                .ok_or_else(|| anyhow::anyhow!("Provider {} 不存在", provider_name))?;
            match provider.stream(request.clone()).await {
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
                    match provider.stream(request.clone()).await {
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
    async fn stream_to_bus(
        &mut self,
        request_id: &str,
        model: &str,
        mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = anyhow::Result<StreamChunk>> + Send>>,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        tracing::debug!("开始采样：model={}, request_id={}", model, request_id);

        // 逐 chunk 读取流式响应
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
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

        // 流式结束，发送 done 消息
        let done_msg = FrameCodec::new_message(
            Topic::sampler_stream(request_id),
            self.id.as_str(),
            &serde_json::json!({
                "type": "done",
                "request_id": request_id,
                "usage": {
                    "prompt_tokens": 0,
                    "completion_tokens": 0,
                    "total_tokens": 0,
                },
                "finish_reason": "stop",
            }),
        )?;
        if let Err(_) = transport.publish_data(&done_msg).await {
            transport.send_message(&done_msg).await?;
        }

        tracing::debug!("采样完成：model={}, request_id={}", model, request_id);
        Ok(())
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

        let mut retry_count: u32 = 0;

        loop {
            match self.try_stream_with_fallback(&request).await {
                Ok((stream, provider_name)) => {
                    tracing::info!("采样开始：model={}, provider={}", model, provider_name);
                    let result = self.stream_to_bus(&request_id, &model, stream, transport).await;
                    // 采样完成，记录推理延迟（从请求到响应的耗时）
                    AgentMetrics::global()
                        .record_inference_latency(start.elapsed().as_millis() as f64);
                    return result;
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
        if msg.topic.as_str() == "sampler/request" {
            let request: SampleRequest = FrameCodec::decode_payload(&msg)?;
            self.handle_sample_request(request, transport).await?;
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["sampler/request".into()]
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
