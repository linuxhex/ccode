//! Sampler 模块 - LLM 采样请求与多 Provider 适配

pub mod provider;
pub mod openai_compat;
pub mod claude_compat;
pub mod gemini;
pub mod router;
pub mod retry;
pub mod token_budget;
pub mod cache_break;

pub use provider::{Provider, SampleRequest, StreamChunk, SampleResponse, TokenUsage, ChatMessage, ToolDefinition as SamplerToolDefinition, ToolChoice, SamplerEvent, CancellationHandle, ContentBlock, ThinkingConfig};
pub use router::ProviderRouter;
pub use retry::{SamplerErrorClass, RetryDecision, classify_sampler_error, retry_backoff_with_jitter, doom_loop_backoff, resolve_max_retries, DEFAULT_MAX_RETRIES, RATE_LIMIT_RETRY_THRESHOLD};
