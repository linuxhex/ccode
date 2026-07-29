//! Sampler 模块 - LLM 采样请求与多 Provider 适配

pub mod provider;
pub mod openai_compat;
pub mod claude_compat;
pub mod deepseek_compat;
pub mod router;
pub mod pool;
pub mod retry;

pub use provider::{Provider, SampleRequest, StreamChunk, SampleResponse, TokenUsage, ChatMessage, ToolDefinition as SamplerToolDefinition, ToolChoice, SamplerEvent, CancellationHandle, ContentBlock};
pub use router::ProviderRouter;
pub use pool::{ConnectionPool, PoolConfig, TokenBucket, LeakyBucket, RetryPolicy, HealthChecker};
pub use retry::{SamplerErrorClass, RetryDecision, classify_sampler_error, retry_backoff_with_jitter, doom_loop_backoff, resolve_max_retries, DEFAULT_MAX_RETRIES, RATE_LIMIT_RETRY_THRESHOLD};
