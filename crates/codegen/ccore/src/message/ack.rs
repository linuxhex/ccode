//! 消息确认和重试机制
//!
//! 提供可靠消息传递：
//! - 消息确认（ACK）
//! - 自动重试（指数退避）
//! - 超时检测
//! - 失败记录
//!
//! 工作流程：
//! 1. 发送方发送消息（带 msg_id）
//! 2. 接收方处理后返回 ACK（包含 msg_id）
//! 3. 如果超时未收到 ACK，触发重试
//! 4. 达到最大重试次数后标记为失败

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};
use tokio::time::sleep;

use crate::message::{Message, MessageHeader};

/// 消息确认配置
#[derive(Debug, Clone)]
pub struct AckConfig {
    /// 确认超时（秒）
    pub ack_timeout_secs: u64,
    /// 最大重试次数
    pub max_retries: usize,
    /// 初始重试延迟（毫秒）
    pub initial_retry_delay_ms: u64,
    /// 最大重试延迟（毫秒）
    pub max_retry_delay_ms: u64,
    /// 重试退避倍数
    pub retry_backoff_multiplier: f64,
}

impl Default for AckConfig {
    fn default() -> Self {
        Self {
            ack_timeout_secs: 30,
            max_retries: 3,
            initial_retry_delay_ms: 1000,
            max_retry_delay_ms: 30000,
            retry_backoff_multiplier: 2.0,
        }
    }
}

/// 待确认的消息
#[derive(Debug, Clone)]
pub struct PendingAck {
    /// 原始消息
    pub message: Message,
    /// 发送时间
    pub sent_at: Instant,
    /// 已重试次数
    pub retry_count: usize,
    /// 下次重试时间
    pub next_retry_at: Instant,
}

/// 消息确认管理器
pub struct AckManager {
    config: AckConfig,
    /// 待确认的消息（msg_id → PendingAck）
    pending: Arc<Mutex<HashMap<String, PendingAck>>>,
    /// 已确认的消息（用于去重）
    confirmed: Arc<Mutex<HashMap<String, Instant>>>,
    /// 失败的消息
    failed: Arc<Mutex<Vec<(Message, String)>>>, // (message, error)
}

impl AckManager {
    pub fn new(config: AckConfig) -> Self {
        Self {
            config,
            pending: Arc::new(Mutex::new(HashMap::new())),
            confirmed: Arc::new(Mutex::new(HashMap::new())),
            failed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 记录已发送消息（等待确认）
    pub async fn record_sent(&self, message: Message) {
        let msg_id = message.header.msg_id.clone();
        let delay = self.calculate_retry_delay(0);
        
        let pending = PendingAck {
            message,
            sent_at: Instant::now(),
            retry_count: 0,
            next_retry_at: Instant::now() + Duration::from_millis(delay),
        };
        
        self.pending.lock().await.insert(msg_id, pending);
    }

    /// 处理确认消息
    pub async fn handle_ack(&self, msg_id: &str) -> bool {
        let mut pending = self.pending.lock().await;
        
        if pending.remove(msg_id).is_some() {
            // 记录已确认
            self.confirmed.lock().await.insert(msg_id.to_string(), Instant::now());
            tracing::debug!("消息已确认：{}", msg_id);
            return true;
        }
        
        false
    }

    /// 检查超时消息并返回需要重试的
    pub async fn check_timeout(&self) -> Vec<Message> {
        let mut pending = self.pending.lock().await;
        let now = Instant::now();
        let timeout = Duration::from_secs(self.config.ack_timeout_secs);
        
        let mut to_retry = Vec::new();
        let mut to_fail = Vec::new();
        
        for (msg_id, p) in pending.iter_mut() {
            // 检查是否超时
            if now.duration_since(p.sent_at) > timeout {
                // 检查是否达到最大重试次数
                if p.retry_count >= self.config.max_retries {
                    to_fail.push((msg_id.clone(), "达到最大重试次数".to_string()));
                } else {
                    // 检查是否到重试时间
                    if now >= p.next_retry_at {
                        to_retry.push(p.message.clone());
                        p.retry_count += 1;
                        p.sent_at = now;
                        let delay = self.calculate_retry_delay(p.retry_count);
                        p.next_retry_at = now + Duration::from_millis(delay);
                    }
                }
            }
        }
        
        // 先从 pending 中移除并收集失败消息，然后释放锁
        let failed_entries: Vec<_> = to_fail.iter()
            .filter_map(|(msg_id, error)| {
                pending.remove(msg_id).map(|p| (p.message, error.clone()))
            })
            .collect();
        drop(pending);

        // pending 锁释放后再获取 failed 锁，避免嵌套锁导致死锁
        let mut failed_guard = self.failed.lock().await;
        for (msg, error) in failed_entries {
            let msg_id = msg.header.msg_id.clone();
            failed_guard.push((msg, error));
            tracing::warn!("消息最终失败：{}", msg_id);
        }

        to_retry
    }

    /// 获取失败的消息列表
    pub async fn get_failed_messages(&self) -> Vec<(Message, String)> {
        self.failed.lock().await.clone()
    }

    /// 清理已确认消息的旧记录
    pub async fn cleanup_old_confirmed(&self, max_age_secs: u64) {
        let mut confirmed = self.confirmed.lock().await;
        let now = Instant::now();
        let max_age = Duration::from_secs(max_age_secs);
        
        confirmed.retain(|_, &mut confirmed_at| {
            now.duration_since(confirmed_at) < max_age
        });
    }

    /// 计算重试延迟（指数退避）
    fn calculate_retry_delay(&self, retry_count: usize) -> u64 {
        let base = self.config.initial_retry_delay_ms as f64;
        let multiplier = self.config.retry_backoff_multiplier.powi(retry_count as i32);
        let delay = base * multiplier;
        
        delay.min(self.config.max_retry_delay_ms as f64) as u64
    }
}

/// 创建确认消息
pub fn create_ack_message(original_msg_id: &str, src_node: &str) -> Message {
    Message {
        topic: crate::message::Topic::new("sys/ack"),
        header: MessageHeader {
            msg_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            src_node: src_node.to_string(),
            reply_to: Some(original_msg_id.to_string()),
            sequence: 0,
            requires_ack: false,
        },
        payload: serde_json::to_vec(&serde_json::json!({
            "original_msg_id": original_msg_id,
            "status": "acknowledged",
        })).unwrap_or_default(),
    }
}

/// 后台重试任务
///
/// 通过 `retry_tx` channel 将需要重试的消息发送给调用方，
/// 由调用方负责实际重发到消息总线。
pub async fn retry_loop(
    ack_manager: Arc<AckManager>,
    retry_tx: mpsc::Sender<Message>,
    mut shutdown_rx: mpsc::Receiver<()>,
) {
    let check_interval = Duration::from_secs(5);

    loop {
        tokio::select! {
            _ = sleep(check_interval) => {
                // 检查超时消息
                let to_retry = ack_manager.check_timeout().await;

                for msg in to_retry {
                    tracing::warn!("重试消息：{}", msg.header.msg_id);
                    // 重新记录为已发送（更新 pending 中的状态）
                    ack_manager.record_sent(msg.clone()).await;
                    // 通过 channel 发送给调用方重发
                    if retry_tx.send(msg).await.is_err() {
                        tracing::warn!("重试通道已关闭，退出重试循环");
                        return;
                    }
                }

                // 清理旧的确认记录
                ack_manager.cleanup_old_confirmed(300).await;
            }

            _ = shutdown_rx.recv() => {
                tracing::info!("重试循环退出");
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Topic;

    #[tokio::test]
    async fn test_ack_flow() {
        let manager = AckManager::new(AckConfig::default());
        
        // 创建测试消息
        let msg = Message {
            topic: Topic::new("test/topic"),
            header: MessageHeader {
                msg_id: "test-msg-1".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                src_node: "test-node".to_string(),
                reply_to: None,
                sequence: 1,
                requires_ack: false,
            },
            payload: vec![],
        };
        
        // 记录发送
        manager.record_sent(msg.clone()).await;
        
        // 检查是否有待确认的消息
        let pending = manager.pending.lock().await;
        assert_eq!(pending.len(), 1);
        drop(pending);
        
        // 处理确认
        let handled = manager.handle_ack("test-msg-1").await;
        assert!(handled);
        
        // 检查待确认列表已清空
        let pending = manager.pending.lock().await;
        assert_eq!(pending.len(), 0);
    }

    #[tokio::test]
    async fn test_retry_delay() {
        let manager = AckManager::new(AckConfig {
            initial_retry_delay_ms: 1000,
            max_retry_delay_ms: 10000,
            retry_backoff_multiplier: 2.0,
            ..Default::default()
        });
        
        // 测试指数退避
        assert_eq!(manager.calculate_retry_delay(0), 1000);
        assert_eq!(manager.calculate_retry_delay(1), 2000);
        assert_eq!(manager.calculate_retry_delay(2), 4000);
        assert_eq!(manager.calculate_retry_delay(3), 8000);
        assert_eq!(manager.calculate_retry_delay(4), 10000); // 达到上限
    }
}