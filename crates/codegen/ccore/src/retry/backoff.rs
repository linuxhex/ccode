//! 指数退避重试实现
//!
//! 提供通用的异步重试函数，支持指数退避策略。

use anyhow::Result;
use std::future::Future;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

/// 重试策略配置
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大重试次数（不含首次调用）
    pub max_retries: u32,
    /// 初始退避时间（毫秒）
    pub initial_backoff_ms: u64,
    /// 最大退避时间（毫秒）
    pub max_backoff_ms: u64,
    /// 可重试的错误标识（如 HTTP 状态码）
    pub retryable_codes: Vec<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_backoff_ms: 1000,
            max_backoff_ms: 30000,
            retryable_codes: vec![401, 408, 429, 500, 502, 503, 504],
        }
    }
}

impl RetryPolicy {
    /// 创建新的重试策略
    pub fn new(max_retries: u32) -> Self {
        Self {
            max_retries,
            ..Default::default()
        }
    }

    /// 计算第 n 次重试的退避时间
    ///
    /// 指数退避：initial_backoff * 2^n，但不超过 max_backoff
    pub fn backoff_duration(&self, attempt: u32) -> Duration {
        let backoff_ms = self
            .initial_backoff_ms
            .saturating_mul(2u64.saturating_pow(attempt))
            .min(self.max_backoff_ms);
        Duration::from_millis(backoff_ms)
    }

    /// 检查错误是否可重试
    pub fn is_retryable(&self, error_code: Option<u16>) -> bool {
        match error_code {
            Some(code) => self.retryable_codes.contains(&code),
            None => true, // 无错误码时默认可重试
        }
    }
}

/// 带指数退避的异步重试函数
///
/// # 参数
/// - policy: 重试策略
/// - f: 异步函数，返回 Result<T>
///
/// # 返回
/// - Ok(result) 首次或某次重试成功
/// - Err(e) 所有重试均失败
///
/// # 示例
/// ```ignore
/// let result = retry_with_backoff(&policy, || async {
///     some_async_operation().await
/// }).await?;
/// ```
pub async fn retry_with_backoff<F, Fut, T>(policy: &RetryPolicy, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(result) => {
                if attempt > 0 {
                    debug!(attempt, "重试成功");
                }
                return Ok(result);
            }
            Err(e) => {
                if attempt >= policy.max_retries {
                    warn!(attempt, error = %e, "重试次数耗尽，放弃");
                    return Err(e);
                }

                let backoff = policy.backoff_duration(attempt);
                warn!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    error = %e,
                    "操作失败，等待后重试"
                );
                sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

/// 带错误码判断的异步重试
///
/// 与 retry_with_backoff 类似，但允许从错误中提取错误码判断是否可重试。
pub async fn retry_with_error_check<F, Fut, T, E>(
    policy: &RetryPolicy,
    mut f: F,
    error_code_fn: impl Fn(&E) -> Option<u16>,
) -> std::result::Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::result::Result<T, E>>,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt >= policy.max_retries {
                    return Err(e);
                }

                let error_code = error_code_fn(&e);
                if !policy.is_retryable(error_code) {
                    return Err(e);
                }

                let backoff = policy.backoff_duration(attempt);
                warn!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    ?error_code,
                    "操作失败（可重试），等待后重试"
                );
                sleep(backoff).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_duration() {
        let policy = RetryPolicy {
            max_retries: 5,
            initial_backoff_ms: 100,
            max_backoff_ms: 10000,
            retryable_codes: vec![],
        };

        assert_eq!(policy.backoff_duration(0), Duration::from_millis(100));
        assert_eq!(policy.backoff_duration(1), Duration::from_millis(200));
        assert_eq!(policy.backoff_duration(2), Duration::from_millis(400));
        assert_eq!(policy.backoff_duration(10), Duration::from_millis(10000));
    }

    #[test]
    fn test_is_retryable() {
        let policy = RetryPolicy::default();
        assert!(policy.is_retryable(Some(503)));
        assert!(!policy.is_retryable(Some(400)));
        assert!(policy.is_retryable(None));
    }
}