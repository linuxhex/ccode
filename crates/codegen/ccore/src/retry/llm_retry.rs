//! LLM 调用重试包装
//!
//! 包装 LLM 采样调用，提供自动重试能力。

use anyhow::Result;
use std::future::Future;
use std::sync::Arc;
use tracing::info;

use super::backoff::{retry_with_backoff, RetryPolicy};

/// LLM 重试配置
#[derive(Debug, Clone)]
pub struct LlmRetryConfig {
    /// 重试策略
    pub policy: RetryPolicy,
    /// 是否启用重试
    pub enabled: bool,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            policy: RetryPolicy {
                max_retries: 3,
                initial_backoff_ms: 2000,
                max_backoff_ms: 30000,
                retryable_codes: vec![401, 429, 500, 502, 503, 504],
            },
            enabled: true,
        }
    }
}

/// 重试 LLM 调用
///
/// # 参数
/// - config: 重试配置
/// - f: LLM 调用异步函数
///
/// # 返回
/// - Ok(result) LLM 调用成功
/// - Err(e) 所有重试均失败
pub async fn retry_llm_call<F, Fut, T>(config: &LlmRetryConfig, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    if !config.enabled {
        return f().await;
    }

    info!(
        max_retries = config.policy.max_retries,
        "LLM 调用重试已启用"
    );

    retry_with_backoff(&config.policy, f).await
}

/// 创建默认的 LLM 重试配置
pub fn default_llm_retry_config() -> Arc<LlmRetryConfig> {
    Arc::new(LlmRetryConfig::default())
}

/// 从环境变量创建 LLM 重试配置
pub fn llm_retry_config_from_env() -> LlmRetryConfig {
    let mut config = LlmRetryConfig::default();

    if let Ok(max) = std::env::var("CCODE_LLM_MAX_RETRIES") {
        if let Ok(max) = max.parse::<u32>() {
            config.policy.max_retries = max;
        }
    }

    if let Ok(initial) = std::env::var("CCODE_LLM_INITIAL_BACKOFF_MS") {
        if let Ok(initial) = initial.parse::<u64>() {
            config.policy.initial_backoff_ms = initial;
        }
    }

    if let Ok(max_backoff) = std::env::var("CCODE_LLM_MAX_BACKOFF_MS") {
        if let Ok(max_backoff) = max_backoff.parse::<u64>() {
            config.policy.max_backoff_ms = max_backoff;
        }
    }

    config
}