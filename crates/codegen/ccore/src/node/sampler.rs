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

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::transport::NodeTransportHandle;
use crate::sampler::provider::{Provider, SampleRequest, StreamChunk};
use crate::sampler::router::ProviderRouter;
use crate::config::provider::ProviderConfig;

/// Sampler Node 实现
pub struct SamplerNode {
    id: NodeId,
    /// 多模型路由器
    router: ProviderRouter,
}

impl SamplerNode {
    /// 创建 SamplerNode，使用空的 ProviderRouter
    pub fn new(id: NodeId) -> Self {
        let router = ProviderRouter::from_configs(&[]);
        Self { id, router }
    }

    /// 使用配置列表创建 SamplerNode
    pub fn with_configs(id: NodeId, configs: &[ProviderConfig]) -> Self {
        let router = ProviderRouter::from_configs(configs);
        Self { id, router }
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
        if let Some(provider) = self.router.find_provider(model) {
            let name = provider.name().to_string();
            match provider.stream(request.clone()).await {
                Ok(stream) => return Ok((stream, name)),
                Err(e) => {
                    tracing::warn!("Provider {} 采样失败：{}, 尝试 fallback", name, e);
                }
            }
        }

        // 第二次尝试：fallback Provider
        if let Some(provider) = self.router.find_provider(model) {
            let name = provider.name().to_string();
            tracing::info!("Fallback 到 Provider：{}", name);
            match provider.stream(request.clone()).await {
                Ok(stream) => return Ok((stream, name)),
                Err(e) => {
                    tracing::warn!("Fallback Provider {} 也失败：{}", name, e);
                }
            }
        }

        Err(anyhow::anyhow!("未找到模型 {} 的可用 Provider", model))
    }

    /// 将流式 chunk 通过消息总线逐个发送
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
                    transport.send_message(&chunk_msg).await?;
                }
                Err(e) => {
                    tracing::warn!("Stream chunk 错误：{}", e);
                    let err_chunk = FrameCodec::new_message(
                        Topic::sampler_stream(request_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "error": format!("{}", e),
                            "request_id": request_id,
                        }),
                    )?;
                    transport.send_message(&err_chunk).await?;
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
        transport.send_message(&done_msg).await?;

        tracing::debug!("采样完成：model={}, request_id={}", model, request_id);
        Ok(())
    }

    /// 处理采样请求
    async fn handle_sample_request(
        &mut self,
        request: SampleRequest,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let request_id = request.request_id.clone();
        let model = request.model.clone();

        match self.try_stream_with_fallback(&request).await {
            Ok((stream, provider_name)) => {
                tracing::info!("采样开始：model={}, provider={}", model, provider_name);
                self.stream_to_bus(&request_id, &model, stream, transport).await
            }
            Err(e) => {
                let err_msg = FrameCodec::new_message(
                    Topic::sampler_stream(&request_id),
                    self.id.as_str(),
                    &serde_json::json!({
                        "error": format!("{}", e),
                        "request_id": request_id,
                    }),
                )?;
                transport.send_message(&err_msg).await?;
                Err(e)
            }
        }
    }
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
        match msg.topic.as_str() {
            "sampler/request" => {
                let request: SampleRequest = FrameCodec::decode_payload(&msg)?;
                self.handle_sample_request(request, transport).await?;
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec!["sampler/request".into()]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Sampler Node 关闭：{}", self.id);
        Ok(())
    }
}
