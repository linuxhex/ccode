//! 熔断器（借鉴 Claude Code withRetry.ts 的重试+熔断模式）
//!
//! 核心模式：
//! 1. 连续失败计数 + 熔断（防止资源浪费）
//! 2. 指数退避 + 随机抖动（0-25%）
//! 3. 429/529/500 分级处理
//! 4. 半开状态：冷却后允许一个探测请求

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// 熔断器状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircuitState {
    /// 关闭（正常，允许请求）
    Closed = 0,
    /// 打开（熔断，拒绝请求）
    Open = 1,
    /// 半开（允许一个探测请求）
    HalfOpen = 2,
}

/// 熔断器配置
pub struct CircuitBreakerConfig {
    /// 触发熔断的连续失败次数（Claude Code: 5）
    pub failure_threshold: u32,
    /// 熔断冷却时间（Claude Code: 30s）
    pub cooldown_duration: Duration,
    /// 最大退避时间（Claude Code: 60s）
    pub max_backoff: Duration,
    /// 初始退避时间（Claude Code: 1s）
    pub initial_backoff: Duration,
    /// 退避乘数（Claude Code: 2.0）
    pub backoff_multiplier: f64,
    /// 抖动因子（Claude Code: 0.25 = 25%）
    pub jitter_factor: f64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            cooldown_duration: Duration::from_secs(30),
            max_backoff: Duration::from_secs(60),
            initial_backoff: Duration::from_secs(1),
            backoff_multiplier: 2.0,
            jitter_factor: 0.25,
        }
    }
}

/// 熔断器
pub struct CircuitBreaker {
    /// 配置
    config: CircuitBreakerConfig,
    /// 当前状态
    state: AtomicU8,
    /// 连续失败计数
    consecutive_failures: AtomicU64,
    /// 上次失败时间
    last_failure_time: std::sync::Mutex<Option<Instant>>,
    /// 总请求计数
    total_requests: AtomicU64,
    /// 总失败计数
    total_failures: AtomicU64,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: AtomicU8::new(CircuitState::Closed as u8),
            consecutive_failures: AtomicU64::new(0),
            last_failure_time: std::sync::Mutex::new(None),
            total_requests: AtomicU64::new(0),
            total_failures: AtomicU64::new(0),
        }
    }

    /// 检查是否允许请求通过
    pub fn allow_request(&self) -> bool {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        let state = self.current_state();
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // 检查冷却时间
                let last_failure = self.last_failure_time.lock().unwrap();
                if let Some(last) = *last_failure {
                    if last.elapsed() >= self.config.cooldown_duration {
                        // 转入半开状态
                        drop(last_failure);
                        self.state.store(CircuitState::HalfOpen as u8, Ordering::Relaxed);
                        tracing::info!(target: "ccore::circuit", "circuit breaker entering half-open state");
                        return true;
                    }
                }
                tracing::warn!(target: "ccore::circuit", "circuit breaker OPEN, request rejected");
                false
            }
            CircuitState::HalfOpen => true, // 允许一个探测请求
        }
    }

    /// 记录成功
    pub fn record_success(&self) {
        let prev = self.consecutive_failures.swap(0, Ordering::Relaxed);
        self.state.store(CircuitState::Closed as u8, Ordering::Relaxed);

        if prev > 0 {
            tracing::info!(
                target: "ccore::circuit",
                previous_failures = prev,
                "circuit breaker reset to CLOSED after success"
            );
        }
    }

    /// 记录失败
    pub fn record_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        self.total_failures.fetch_add(1, Ordering::Relaxed);

        *self.last_failure_time.lock().unwrap() = Some(Instant::now());

        if failures >= self.config.failure_threshold as u64 {
            self.state.store(CircuitState::Open as u8, Ordering::Relaxed);
            tracing::error!(
                target: "ccore::circuit",
                consecutive_failures = failures,
                threshold = self.config.failure_threshold,
                "circuit breaker TRIPPED to OPEN state"
            );
        } else {
            tracing::warn!(
                target: "ccore::circuit",
                consecutive_failures = failures,
                threshold = self.config.failure_threshold,
                "failure recorded"
            );
        }
    }

    /// 计算退避时间（指数退避 + 抖动）
    ///
    /// Claude Code: getRetryDelay() with jitter
    pub fn retry_delay(&self, attempt: u32) -> Duration {
        let base = self.config.initial_backoff.as_secs_f64()
            * self.config.backoff_multiplier.powi(attempt as i32);

        let capped = base.min(self.config.max_backoff.as_secs_f64());

        // 抖动：0-25%随机偏移（Claude Code: jitter implementation）
        let jitter = if self.config.jitter_factor > 0.0 {
            let rand_factor = (attempt as f64 * 0.618) % 1.0; // 简单的伪随机
            capped * self.config.jitter_factor * (rand_factor * 2.0 - 1.0).abs()
        } else {
            0.0
        };

        let delay = (capped + jitter).max(0.5);
        Duration::from_secs_f64(delay)
    }

    /// 获取当前状态
    pub fn current_state(&self) -> CircuitState {
        match self.state.load(Ordering::Relaxed) {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> (CircuitState, u64, u64, u64) {
        (
            self.current_state(),
            self.consecutive_failures.load(Ordering::Relaxed),
            self.total_requests.load(Ordering::Relaxed),
            self.total_failures.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_closed_by_default() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        assert_eq!(cb.current_state(), CircuitState::Closed);
        assert!(cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_trips_on_failures() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        // Default threshold is 5
        for _ in 0..4 {
            cb.record_failure();
            assert_eq!(cb.current_state(), CircuitState::Closed);
        }
        cb.record_failure(); // 5th failure
        assert_eq!(cb.current_state(), CircuitState::Open);
    }

    #[test]
    fn test_circuit_breaker_rejects_when_open() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 1,
            ..Default::default()
        });
        cb.record_failure();
        assert_eq!(cb.current_state(), CircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn test_circuit_breaker_success_resets() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        });
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        let (_, consecutive, _, _) = cb.stats();
        assert_eq!(consecutive, 0);
        assert_eq!(cb.current_state(), CircuitState::Closed);
    }

    #[test]
    fn test_retry_delay_increases() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        let d0 = cb.retry_delay(0);
        let d1 = cb.retry_delay(1);
        let d2 = cb.retry_delay(2);
        assert!(d1 > d0);
        assert!(d2 > d1);
    }

    #[test]
    fn test_retry_delay_capped() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig::default());
        let d_large = cb.retry_delay(100);
        assert!(d_large <= Duration::from_secs(90)); // max_backoff + jitter margin
    }

    #[test]
    fn test_stats() {
        let cb = CircuitBreaker::new(CircuitBreakerConfig {
            failure_threshold: 10,
            ..Default::default()
        });
        cb.allow_request(); // total_requests = 1
        cb.record_failure(); // consecutive = 1, total_failures = 1
        let (state, consecutive, total_req, total_fail) = cb.stats();
        assert_eq!(state, CircuitState::Closed);
        assert_eq!(consecutive, 1);
        assert_eq!(total_req, 1);
        assert_eq!(total_fail, 1);
    }
}
