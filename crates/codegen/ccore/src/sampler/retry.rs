//! 采样器重试分类与退避策略（借鉴 Claude Code ccode-sampler/retry.rs）

use std::time::Duration;

/// 默认最大重试次数
pub const DEFAULT_MAX_RETRIES: u32 = 15;

/// 429 速率限制重试阈值
pub const RATE_LIMIT_RETRY_THRESHOLD: u32 = 2;

/// 采样错误分类（简化版，适配 ccore 的错误模型）
#[derive(Debug, Clone)]
pub enum SamplerErrorClass {
    /// 可重试：5xx、连接错误、空响应
    Retryable { status_code: Option<u16> },
    /// 速率限制（429）
    RateLimited { retry_after_secs: Option<u64> },
    /// 上下文窗口溢出（不可重试）
    ContextOverflow,
    /// 认证错误（不可重试）
    AuthError,
    /// 客户端错误（4xx，不可重试）
    ClientError { status_code: u16 },
    /// 空响应（可重试）
    EmptyResponse,
    /// 流式传输错误（可重试）
    StreamError,
    /// Doom Loop 检测（可重试，近立即退避）
    DoomLoop,
}

/// 重试决策
#[derive(Debug, Clone)]
pub enum RetryDecision {
    /// 指数退避重试
    Retry { backoff: Duration },
    /// 速率限制重试
    RetryWithBackoff { backoff: Duration, is_rate_limited: bool },
    /// Doom Loop 近立即重试
    RetryImmediate { backoff: Duration },
    /// 不可重试，致命错误
    Fatal(String),
}

/// 分类采样错误并决定是否重试
pub fn classify_sampler_error(
    error_message: &str,
    status_code: Option<u16>,
    retry_count: u32,
    max_retries: u32,
) -> RetryDecision {
    // Check auth errors first
    if let Some(code) = status_code {
        match code {
            401 | 403 => return RetryDecision::Fatal(format!("认证失败（HTTP {}）", code)),
            400 | 404 | 422 => return RetryDecision::Fatal(format!("客户端错误（HTTP {}）", code)),
            429 => {
                let next = retry_count + 1;
                if next >= RATE_LIMIT_RETRY_THRESHOLD {
                    return RetryDecision::Fatal("速率限制重试次数耗尽".into());
                }
                let backoff = retry_backoff_with_jitter(next);
                return RetryDecision::RetryWithBackoff { backoff, is_rate_limited: true };
            }
            500 | 502 | 503 | 504 => {
                let next = retry_count + 1;
                if next >= max_retries {
                    return RetryDecision::Fatal(format!("服务端错误重试耗尽（HTTP {}）", code));
                }
                let backoff = retry_backoff_with_jitter(next);
                return RetryDecision::Retry { backoff };
            }
            _ => {}
        }
    }

    // Check error message patterns
    let msg_lower = error_message.to_lowercase();
    if msg_lower.contains("context") && (msg_lower.contains("too long") || msg_lower.contains("overflow")) {
        return RetryDecision::Fatal("上下文窗口溢出".into());
    }
    if msg_lower.contains("timeout") || msg_lower.contains("connection") {
        let next = retry_count + 1;
        if next >= max_retries {
            return RetryDecision::Fatal("连接错误重试耗尽".into());
        }
        return RetryDecision::Retry { backoff: retry_backoff_with_jitter(next) };
    }
    if msg_lower.contains("empty response") || msg_lower.contains("no content") {
        let next = retry_count + 1;
        if next >= max_retries {
            return RetryDecision::Fatal("空响应重试耗尽".into());
        }
        return RetryDecision::Retry { backoff: retry_backoff_with_jitter(next) };
    }
    if msg_lower.contains("doom loop") || msg_lower.contains("loop detected") {
        return RetryDecision::RetryImmediate { backoff: doom_loop_backoff(retry_count) };
    }

    // Default: fatal
    RetryDecision::Fatal(format!("不可重试的错误：{}", error_message))
}

/// 指数退避（2s, 4s, 8s, ..., 上限 30s）+/-20% 抖动
/// 直接借鉴 Claude Code 的 retry_backoff_with_jitter
pub fn retry_backoff_with_jitter(retry_count: u32) -> Duration {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static JITTER_SEQ: AtomicU64 = AtomicU64::new(0);

    let shift = retry_count.saturating_sub(1);
    let base_ms = 2000u64.checked_shl(shift).unwrap_or(u64::MAX).min(30_000);
    let jitter_range = base_ms / 5;
    let mut hasher = std::hash::DefaultHasher::new();
    JITTER_SEQ.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let jitter = hasher.finish() % (jitter_range * 2 + 1);
    Duration::from_millis(base_ms - jitter_range + jitter)
}

/// Doom Loop 近立即退避（直接借鉴 Claude Code 的 doom_loop_backoff）
pub fn doom_loop_backoff(retry_count: u32) -> Duration {
    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static JITTER_SEQ: AtomicU64 = AtomicU64::new(0);

    let mut hasher = std::hash::DefaultHasher::new();
    JITTER_SEQ.fetch_add(1, Ordering::Relaxed).hash(&mut hasher);
    retry_count.hash(&mut hasher);
    Duration::from_millis(hasher.finish() % 251)
}

/// 从环境变量解析最大重试次数
pub fn resolve_max_retries(model_max_retries: Option<u32>) -> u32 {
    std::env::var("CCODE_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .or(model_max_retries)
        .unwrap_or(DEFAULT_MAX_RETRIES)
}
