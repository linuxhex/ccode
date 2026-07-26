//! 连接池与高级限流策略
//!
//! 提供HTTP连接复用、高级限流（令牌桶 + 漏桶）、重试策略、健康检查

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Semaphore};
use tokio::time::sleep;

/// HTTP 连接池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 最大连接数
    pub max_connections: usize,
    /// 连接空闲超时（秒）
    pub idle_timeout_secs: u64,
    /// 连接存活超时（秒）
    pub lifetime_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 100,
            idle_timeout_secs: 90,
            lifetime_secs: 300,
        }
    }
}

/// HTTP 连接池
pub struct ConnectionPool {
    #[allow(dead_code)]
    config: PoolConfig,
    /// 并发连接数信号量
    semaphore: Arc<Semaphore>,
    /// 总请求数统计
    total_requests: AtomicU64,
    /// 成功请求数统计
    success_requests: AtomicU64,
    /// 失败请求数统计
    failed_requests: AtomicU64,
}

impl ConnectionPool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(config.max_connections)),
            total_requests: AtomicU64::new(0),
            success_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            config,
        }
    }

    /// 获取连接许可（异步等待）
    pub async fn acquire(&self) -> anyhow::Result<ConnectionGuard<'_>> {
        let permit = self.semaphore.acquire().await
            .map_err(|_| anyhow::anyhow!("ConnectionPool semaphore 已关闭"))?;
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        Ok(ConnectionGuard {
            permit,
            pool: self,
        })
    }

    /// 记录成功请求
    fn record_success(&self) {
        self.success_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录失败请求
    fn record_failure(&self) {
        self.failed_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取统计信息
    pub fn stats(&self) -> PoolStats {
        PoolStats {
            total_requests: self.total_requests.load(Ordering::Relaxed),
            success_requests: self.success_requests.load(Ordering::Relaxed),
            failed_requests: self.failed_requests.load(Ordering::Relaxed),
            available_connections: self.semaphore.available_permits(),
        }
    }
}

/// 连接许可守卫
pub struct ConnectionGuard<'a> {
    #[allow(dead_code)]
    permit: tokio::sync::SemaphorePermit<'a>,
    pool: &'a ConnectionPool,
}

impl<'a> ConnectionGuard<'a> {
    /// 标记请求成功
    pub fn success(self) {
        self.pool.record_success();
    }

    /// 标记请求失败
    pub fn failure(self) {
        self.pool.record_failure();
    }
}

/// 连接池统计信息
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub total_requests: u64,
    pub success_requests: u64,
    pub failed_requests: u64,
    pub available_connections: usize,
}

/// 令牌桶限流器
pub struct TokenBucket {
    /// 令牌容量
    capacity: u64,
    /// 当前令牌数
    tokens: AtomicU64,
    /// 每秒补充令牌数
    refill_rate: u64,
    /// 上次补充时间
    last_refill: Mutex<Instant>,
}

impl TokenBucket {
    pub fn new(capacity: u64, refill_rate: u64) -> Self {
        Self {
            capacity,
            tokens: AtomicU64::new(capacity),
            refill_rate,
            last_refill: Mutex::new(Instant::now()),
        }
    }

    /// 尝试获取令牌
    pub async fn try_acquire(&self) -> bool {
        self.refill().await;
        
        loop {
            let current = self.tokens.load(Ordering::Relaxed);
            if current == 0 {
                return false;
            }
            
            if self.tokens.compare_exchange(
                current,
                current - 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ).is_ok() {
                return true;
            }
        }
    }

    /// 补充令牌
    async fn refill(&self) {
        let mut last = self.last_refill.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        
        if elapsed >= Duration::from_secs(1) {
            let tokens_to_add = elapsed.as_secs() * self.refill_rate;
            let current = self.tokens.load(Ordering::Relaxed);
            let new_tokens = (current + tokens_to_add).min(self.capacity);
            self.tokens.store(new_tokens, Ordering::Release);
            *last = now;
        }
    }

    /// 等待获取令牌
    pub async fn acquire(&self) {
        while !self.try_acquire().await {
            sleep(Duration::from_millis(100)).await;
        }
    }
}

/// 漏桶限流器（固定速率）
pub struct LeakyBucket {
    /// 速率（请求/秒）
    rate: f64,
    /// 上次请求时间
    last_request: Mutex<Instant>,
}

impl LeakyBucket {
    pub fn new(rate: f64) -> Self {
        Self {
            rate,
            last_request: Mutex::new(Instant::now()),
        }
    }

    /// 等待直到可以发送请求
    pub async fn wait(&self) {
        let interval = Duration::from_secs_f64(1.0 / self.rate);
        
        let mut last = self.last_request.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(*last);
        
        if elapsed < interval {
            sleep(interval - elapsed).await;
        }
        
        *last = Instant::now();
    }
}

/// 重试策略
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最大重试次数
    pub max_retries: usize,
    /// 初始延迟（毫秒）
    pub initial_delay_ms: u64,
    /// 最大延迟（毫秒）
    pub max_delay_ms: u64,
    /// 指数退避基数
    pub multiplier: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 30000,
            multiplier: 2.0,
        }
    }
}

impl RetryPolicy {
    /// 执行带重试的异步操作
    pub async fn execute<F, T, E>(&self, mut f: F) -> Result<T, E>
    where
        F: FnMut() -> std::pin::Pin<std::boxed::Box<dyn std::future::Future<Output = Result<T, E>> + Send>> + Send,
        E: std::fmt::Debug,
    {
        let mut last_error = None;
        let mut delay = Duration::from_millis(self.initial_delay_ms);

        for attempt in 0..=self.max_retries {
            match f().await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    if attempt == self.max_retries {
                        return Err(e);
                    }
                    
                    tracing::warn!(
                        "请求失败（尝试 {}/{}），{}ms 后重试：{:?}",
                        attempt + 1,
                        self.max_retries,
                        delay.as_millis(),
                        e
                    );
                    
                    sleep(delay).await;
                    
                    // 指数退避
                    delay = Duration::from_millis(
                        (delay.as_millis() as f64 * self.multiplier).min(self.max_delay_ms as f64) as u64
                    );
                    
                    last_error = Some(e);
                }
            }
        }

        Err(last_error.expect("所有重试均失败但没有捕获到错误"))
    }
}

/// 健康检查器
pub struct HealthChecker {
    /// 健康检查间隔
    check_interval: Duration,
    /// 上次检查时间
    last_check: Mutex<Instant>,
    /// 健康状态
    is_healthy: std::sync::atomic::AtomicBool,
}

impl HealthChecker {
    pub fn new(check_interval_secs: u64) -> Self {
        Self {
            check_interval: Duration::from_secs(check_interval_secs),
            last_check: Mutex::new(Instant::now()),
            is_healthy: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// 检查是否需要执行健康检查
    pub async fn needs_check(&self) -> bool {
        let mut last = self.last_check.lock().await;
        let now = Instant::now();
        
        if now.duration_since(*last) >= self.check_interval {
            *last = now;
            true
        } else {
            false
        }
    }

    /// 标记健康状态
    pub fn set_health(&self, healthy: bool) {
        self.is_healthy.store(healthy, Ordering::Release);
    }

    /// 获取健康状态
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_token_bucket() {
        let bucket = TokenBucket::new(10, 5);
        
        // 初始应该有10个令牌
        assert!(bucket.try_acquire().await);
        
        // 快速消耗令牌
        for _ in 0..9 {
            assert!(bucket.try_acquire().await);
        }
        
        // 令牌应该耗尽
        assert!(!bucket.try_acquire().await);
    }

    #[tokio::test]
    async fn test_connection_pool() {
        let pool = ConnectionPool::new(PoolConfig::default());
        
        // 获取连接
        let guard = pool.acquire().await.unwrap();
        guard.success();
        
        // 检查统计
        let stats = pool.stats();
        assert_eq!(stats.total_requests, 1);
        assert_eq!(stats.success_requests, 1);
    }

    #[tokio::test]
    async fn test_retry_policy() {
        let policy = RetryPolicy {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 1000,
            multiplier: 2.0,
        };
        
        let mut attempts = 0;
        let result = policy.execute(|| {
            attempts += 1;
            Box::pin(async move {
                if attempts < 3 {
                    Err("temporary error")
                } else {
                    Ok("success")
                }
            })
        }).await;
        
        assert!(result.is_ok());
        assert_eq!(attempts, 3);
    }
}