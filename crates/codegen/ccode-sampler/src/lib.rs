//! # ccode-sampler
//!
//! LLM 采样层，支持多 Provider、流式响应与模型切换。
//!
//! | 核心能力 | 说明 |
//! |---|---|
//! | 多 Provider | OpenAI / Anthropic / Ollama 统一适配 |
//! | 流式响应 | SSE 流式 chunk 解析与事件转换 |
//! | 模型切换 | 运行时动态切换 LLM 模型 |
//! | 重试与取消 | 请求级重试、取消与指标采集 |

pub mod actor;
pub mod attribution;
pub mod client;
pub mod commands;
pub mod config;
pub mod doom_loop;
pub mod events;
pub mod handle;
pub mod metrics;
pub mod retry;
pub mod sampling_log;
mod shared_http;
pub mod stream;
pub mod types;

// Public re-exports — the API surface consumers see.
pub use actor::SamplerActor;
pub use attribution::{
    Auth401AttributionCallback, SENT_BEARER_PREFIX_LEN, SamplingConsumer, SharedAttributionCallback,
};
pub use client::{ApiBackend, SamplingClient, user_agent_string_for};
pub use config::{
    AuthScheme, BearerResolver, HeaderInjector, OriginClientInfo, RetryPolicy, SamplerConfig,
    SharedBearerResolver, SharedHeaderInjector,
};
pub use doom_loop::DoomLoopSignalCollector;
pub use events::{SamplingChannel, SamplingErrorInfo, SamplingErrorKind, SamplingEvent};
pub use handle::SamplerHandle;
pub use metrics::{InferenceLatencyStats, compute_percentiles};
pub use retry::{
    DEFAULT_MAX_RETRIES, RATE_LIMIT_RETRY_THRESHOLD, RetryDecision, classify_error,
    format_sampling_error, resolve_max_retries, retry_backoff_with_jitter,
};
pub use sampling_log::AuthInfo;
pub use stream::{collect_response, stream_chat_completions, stream_messages, stream_responses};
pub use types::RequestId;
