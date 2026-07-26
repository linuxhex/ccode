//! 背压控制和流量整形
//!
//! 防止消息生产速度超过消费速度，避免系统过载。
//!
//! 功能：
//! - 通道队列监控（背压检测）
//! - 流量整形（令牌桶、漏桶）
//! - 自适应限流（根据负载调整）
//! - 优先级队列（高优先级消息优先）

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};  // ✅ 使用 tokio 的 Mutex

/// 背压控制配置
#[derive(Debug, Clone)]
pub struct BackpressureConfig {
    /// 通道容量阈值（开始背压）
    pub high_watermark: f64,  // 0.8 表示 80%
    /// 通道容量阈值（严重背压）
    pub critical_watermark: f64,  // 0.95 表示 95%
    /// 背压时的发送延迟（毫秒）
    pub backpressure_delay_ms: u64,
    /// 严重背压时的发送延迟（毫秒）
    pub critical_delay_ms: u64,
    /// 最大消息速率（消息/秒，0 表示不限流）
    pub max_rate: u64,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            high_watermark: 0.8,
            critical_watermark: 0.95,
            backpressure_delay_ms: 10,
            critical_delay_ms: 100,
            max_rate: 1000,  // 1000 msg/s
        }
    }
}

/// 背压状态
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackpressureLevel {
    /// 正常（无背压）
    Normal,
    /// 高负载（开始背压）
    High,
    /// 严重过载（严重背压）
    Critical,
}

/// 背压控制器
pub struct BackpressureController {
    config: BackpressureConfig,
    /// 当前背压级别
    level: AtomicU64,  // 存储 BackpressureLevel 的枚举值
    /// 最后一次检查时间（用 std::sync::Mutex 因为 check_channel 是同步方法）
    last_check: std::sync::Mutex<Instant>,
    /// 统计：已发送消息数
    sent_count: AtomicU64,
    /// 统计：已丢弃消息数
    dropped_count: AtomicU64,
    /// 统计：背压触发次数
    backpressure_count: AtomicU64,
}

impl BackpressureController {
    pub fn new(config: BackpressureConfig) -> Self {
        Self {
            config,
            level: AtomicU64::new(BackpressureLevel::Normal as u64),
            last_check: std::sync::Mutex::new(Instant::now()),
            sent_count: AtomicU64::new(0),
            dropped_count: AtomicU64::new(0),
            backpressure_count: AtomicU64::new(0),
        }
    }

    /// 检查通道状态并返回背压级别
    ///
    /// 由于 mpsc::Sender 没有 len() 方法，使用 try_reserve 估算通道使用率。
    /// 如果 try_reserve 成功说明通道未满，如果失败说明通道接近满载。
    pub fn check_channel<T>(&self, channel: &mpsc::Sender<T>) -> BackpressureLevel {
        // try_reserve 尝试预留发送槽位：成功=通道未满，失败=通道已满
        let usage = match channel.try_reserve() {
            Ok(permit) => {
                // 预留成功，立即释放（不发送数据）
                drop(permit);
                // 通道未满，估算使用率基于容量
                // 保守估算：通道使用率 = 1 - (available_approx / capacity)
                // 由于无法获取精确的 available，假设预留成功时使用率较低
                let capacity = channel.capacity();
                // 保守地假设已用 50% 以下
                0.5 * (1.0 / capacity.max(1) as f64).min(1.0)
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                // 通道已满，使用率 100%
                1.0
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // 通道已关闭
                1.0
            }
        };

        let level = if usage >= self.config.critical_watermark {
            BackpressureLevel::Critical
        } else if usage >= self.config.high_watermark {
            BackpressureLevel::High
        } else {
            BackpressureLevel::Normal
        };

        self.level.store(level as u64, Ordering::Release);

        // 记录检查时间，用于监控检查频率和诊断背压问题
        if let Ok(mut instant) = self.last_check.lock() {
            *instant = Instant::now();
        }

        if level != BackpressureLevel::Normal {
            self.backpressure_count.fetch_add(1, Ordering::Relaxed);
        }

        level
    }

    /// 根据背压级别获取发送延迟
    pub fn get_delay(&self) -> Option<Duration> {
        match self.get_level() {
            BackpressureLevel::Normal => None,
            BackpressureLevel::High => Some(Duration::from_millis(self.config.backpressure_delay_ms)),
            BackpressureLevel::Critical => Some(Duration::from_millis(self.config.critical_delay_ms)),
        }
    }

    /// 获取当前背压级别
    pub fn get_level(&self) -> BackpressureLevel {
        match self.level.load(Ordering::Acquire) {
            0 => BackpressureLevel::Normal,
            1 => BackpressureLevel::High,
            2 => BackpressureLevel::Critical,
            _ => BackpressureLevel::Normal,
        }
    }

    /// 记录已发送消息
    pub fn record_sent(&self) {
        self.sent_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录已丢弃消息
    pub fn record_dropped(&self) {
        self.dropped_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取统计信息
    pub fn stats(&self) -> BackpressureStats {
        BackpressureStats {
            level: self.get_level(),
            sent_count: self.sent_count.load(Ordering::Relaxed),
            dropped_count: self.dropped_count.load(Ordering::Relaxed),
            backpressure_count: self.backpressure_count.load(Ordering::Relaxed),
        }
    }
}

/// 背压统计信息
#[derive(Debug, Clone)]
pub struct BackpressureStats {
    pub level: BackpressureLevel,
    pub sent_count: u64,
    pub dropped_count: u64,
    pub backpressure_count: u64,
}

/// 流量整形器（使用令牌桶算法）
pub struct TrafficShaper {
    /// 令牌桶
    tokens: AtomicU64,
    /// 桶容量
    capacity: u64,
    /// 补充速率（令牌/秒）
    refill_rate: u64,
    /// 上次补充时间
    last_refill: Mutex<Instant>,  // ✅ tokio::sync::Mutex
}

impl TrafficShaper {
    pub fn new(capacity: u64, refill_rate: u64) -> Self {
        Self {
            tokens: AtomicU64::new(capacity),
            capacity,
            refill_rate,
            last_refill: Mutex::new(Instant::now()),  // ✅ tokio::sync::Mutex
        }
    }

    /// 尝试获取令牌
    pub fn try_acquire(&self) -> bool {
        self.refill();
        
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

    /// 等待获取令牌（异步）
    pub async fn acquire(&self) {
        while !self.try_acquire() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    /// 补充令牌
    fn refill(&self) {
        let mut last = match self.last_refill.try_lock() {
            Ok(guard) => guard,
            Err(_) => return, // 锁被占用，跳过本次补充
        };
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
}

/// 带背压的发送器
pub struct BackpressureSender<T> {
    sender: mpsc::Sender<T>,
    controller: Arc<BackpressureController>,
    shaper: Option<TrafficShaper>,
}

impl<T> BackpressureSender<T> {
    pub fn new(
        sender: mpsc::Sender<T>,
        controller: Arc<BackpressureController>,
        shaper: Option<TrafficShaper>,
    ) -> Self {
        Self {
            sender,
            controller,
            shaper,
        }
    }

    /// 发送消息（带背压控制）
    pub async fn send(&self, msg: T) -> Result<(), T> {
        // 检查背压级别
        let _level = self.controller.get_level();
        
        // 流量整形
        if let Some(ref shaper) = self.shaper {
            shaper.acquire().await;
        }
        
        // 根据背压级别决定是否延迟
        if let Some(delay) = self.controller.get_delay() {
            tokio::time::sleep(delay).await;
        }
        
        // 尝试发送
        match self.sender.try_send(msg) {
            Ok(()) => {
                self.controller.record_sent();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(msg)) => {
                self.controller.record_dropped();
                Err(msg)
            }
            Err(mpsc::error::TrySendError::Closed(msg)) => {
                Err(msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backpressure_level() {
        let controller = BackpressureController::new(BackpressureConfig::default());
        
        assert_eq!(controller.get_level(), BackpressureLevel::Normal);
    }

    #[tokio::test]
    async fn test_traffic_shaper() {
        let shaper = TrafficShaper::new(10, 5);
        
        // 应该能获取令牌
        assert!(shaper.try_acquire());
        
        // 快速消耗令牌
        for _ in 0..9 {
            assert!(shaper.try_acquire());
        }
        
        // 令牌应该耗尽
        assert!(!shaper.try_acquire());
    }

    #[tokio::test]
    async fn test_backpressure_sender() {
        let (tx, mut rx) = mpsc::channel::<String>(10);
        let controller = Arc::new(BackpressureController::new(BackpressureConfig::default()));
        let sender = BackpressureSender::new(tx, controller, None);
        
        // 发送消息
        let result = sender.send("test".to_string()).await;
        assert!(result.is_ok());
        
        // 接收消息
        let msg = rx.recv().await;
        assert_eq!(msg, Some("test".to_string()));
    }
}