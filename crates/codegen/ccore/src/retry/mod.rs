//! 重试模块入口（已迁移至仿生架构）
//!
//! [DEPRECATED] 重试/退避机制已融合到 LimbNode（肌肉记忆）中。
//! 新代码请使用 LimbNode 的 retry 逻辑替代。
//! 本模块保留以兼容现有调用方。

pub mod backoff;
pub mod llm_retry;
pub mod circuit_breaker;

pub use backoff::{RetryPolicy, retry_with_backoff};
pub use llm_retry::retry_llm_call;