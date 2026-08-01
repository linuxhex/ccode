//! 基于消息总线的 LLM 摘要器
//!
//! 通过 AgentNode → SamplerNode 消息链路调用 LLM 生成摘要，
//! 是 WorkingMemory 的 LlmSummarizer trait 的生产级实现。

use tokio::sync::oneshot;

use super::working::LlmSummarizer;

/// LLM 摘要请求（通过内部通道发送给 AgentNode）
#[derive(Debug)]
pub struct SummarizeRequest {
    /// 摘要提示词
    pub prompt: String,
    /// 需要摘要的内容
    pub content: String,
    /// 响应通道
    pub response_tx: oneshot::Sender<Result<String, anyhow::Error>>,
}

/// 基于消息总线的 LLM 摘要器
///
/// 通过内部 mpsc 通道将摘要请求发送到 AgentNode，
/// AgentNode 再通过消息总线转发给 SamplerNode，
/// SamplerNode 调用 LLM 后将结果返回。
pub struct MessageBusSummarizer {
    /// 摘要请求发送端
    request_tx: tokio::sync::mpsc::Sender<SummarizeRequest>,
}

impl MessageBusSummarizer {
    /// 创建新的消息总线摘要器
    ///
    /// # Arguments
    /// * `request_tx` - 发送到 AgentNode 的摘要请求通道
    pub fn new(request_tx: tokio::sync::mpsc::Sender<SummarizeRequest>) -> Self {
        Self { request_tx }
    }
}

#[async_trait::async_trait]
impl LlmSummarizer for MessageBusSummarizer {
    async fn summarize(&self, prompt: &str, content: &str) -> Result<String, anyhow::Error> {
        let (response_tx, response_rx) = oneshot::channel();

        let request = SummarizeRequest {
            prompt: prompt.to_string(),
            content: content.to_string(),
            response_tx,
        };

        self.request_tx.send(request).await.map_err(|_| {
            anyhow::anyhow!("摘要请求通道已关闭，AgentNode 可能已退出")
        })?;

        response_rx.await.map_err(|_| {
            anyhow::anyhow!("摘要响应通道已关闭，LLM 调用可能已超时")
        })?
    }
}

/// 基于 ccode-sampler 的直接调用摘要器
///
/// 不经过消息总线，直接调用 ccode-sampler 的 API。
/// 适用于单进程模式或测试环境。
pub struct DirectSamplerSummarizer {
    /// 模型名称
    model: String,
    /// 最大 token 数
    max_tokens: u32,
}

impl DirectSamplerSummarizer {
    pub fn new(model: String, max_tokens: u32) -> Self {
        Self { model, max_tokens }
    }
}

#[async_trait::async_trait]
impl LlmSummarizer for DirectSamplerSummarizer {
    async fn summarize(&self, prompt: &str, content: &str) -> Result<String, anyhow::Error> {
        // 构建完整的摘要请求 prompt
        let _full_prompt = format!(
            "{}\n\n{}\n\n请用简洁的中文总结以上内容的要点，不超过 {} 个字：",
            prompt,
            content,
            self.max_tokens
        );

        // 这里应该调用 ccode-sampler 的 API
        // 目前先返回截断结果，待集成 ccode-sampler 后替换
        tracing::debug!(
            model = %self.model,
            content_len = content.len(),
            "DirectSamplerSummarizer 摘要请求（待集成 sampler API）"
        );

        // 回退到截断（当 sampler API 不可用时）
        if content.len() <= self.max_tokens as usize {
            return Ok(content.to_string());
        }
        Ok(format!(
            "[摘要] {}...",
            content.chars().take(self.max_tokens as usize).collect::<String>()
        ))
    }
}
