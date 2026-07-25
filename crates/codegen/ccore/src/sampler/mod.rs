//! Sampler 模块 - LLM 采样请求与多 Provider 适配

pub mod provider;
pub mod openai_compat;
pub mod claude_compat;
pub mod router;

pub use provider::{Provider, SampleRequest, StreamChunk, SampleResponse, TokenUsage, ChatMessage, ToolDefinition as SamplerToolDefinition};
pub use router::ProviderRouter;
