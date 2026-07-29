//! 消息序列号管理
//!
//! 维护每个 Node 的发送/接收序列号，检测乱序消息。
//!
//! 设计：
//! - 每个 Node 维护独立的发送序列号计数器
//! - Kernel 维护每个 Node 的接收序列号窗口（滑动窗口）
//! - 接收方检测序列号跳跃（可能丢包）或回退（乱序）

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::node::NodeId;

/// 序列号管理器（Node 端）
///
/// 每个 Node 维护一个发送序列号计数器。
pub struct SequenceManager {
    /// 当前发送序列号
    send_seq: AtomicU64,
    /// Node ID（用于日志）
    #[allow(dead_code)]
    node_id: NodeId,
}

impl SequenceManager {
    pub fn new(node_id: NodeId) -> Self {
        Self {
            send_seq: AtomicU64::new(0),
            node_id,
        }
    }

    /// 获取下一个发送序列号（自增）
    pub fn next_sequence(&self) -> u64 {
        self.send_seq.fetch_add(1, Ordering::SeqCst)
    }

    /// 获取当前序列号（不递增）
    pub fn current_sequence(&self) -> u64 {
        self.send_seq.load(Ordering::SeqCst)
    }
}

/// 序列号检查器（Kernel 端）
///
/// 维护每个 Node 的接收序列号窗口，检测乱序。
pub struct SequenceChecker {
    /// Node ID → 最后收到的序列号
    last_received: HashMap<NodeId, u64>,
    /// 滑动窗口大小（允许的最大序列号差距）
    window_size: u64,
}

impl SequenceChecker {
    pub fn new(window_size: u64) -> Self {
        Self {
            last_received: HashMap::new(),
            window_size,
        }
    }

    /// 检查消息序列号
    ///
    /// 返回检查结果：
    /// - `Ok(InOrder)`：正常顺序
    /// - `Ok(Gap(n))`：序列号跳跃（可能丢包），但仍接受
    /// - `Err(Duplicate)`：重复消息
    /// - `Err(OutOfOrder)`：序列号回退（乱序），拒绝接受
    pub fn check(&mut self, node_id: &NodeId, sequence: u64) -> Result<SequenceCheckResult, SequenceError> {
        let last_opt = self.last_received.get(node_id).copied();

        match last_opt {
            None => {
                // 首次收到此 Node 的消息，始终接受
                self.last_received.insert(node_id.clone(), sequence);
                Ok(SequenceCheckResult::InOrder)
            }
            Some(last) => {
                if sequence > last {
                    // 正常：序列号递增
                    let gap = sequence - last;
                    self.last_received.insert(node_id.clone(), sequence);

                    if gap > self.window_size {
                        // 序列号跳跃过大，可能丢包
                        tracing::warn!(
                            "Node {} 序列号跳跃：从 {} 到 {}（差距 {}），可能丢包",
                            node_id, last, sequence, gap
                        );
                        Ok(SequenceCheckResult::Gap(gap))
                    } else if gap > 1 {
                        // 序列号跳跃，但可接受
                        tracing::debug!(
                            "Node {} 序列号跳跃：从 {} 到 {}（差距 {}）",
                            node_id, last, sequence, gap
                        );
                        Ok(SequenceCheckResult::Gap(gap))
                    } else {
                        // 完美顺序
                        Ok(SequenceCheckResult::InOrder)
                    }
                } else if sequence == last {
                    // 重复消息
                    tracing::warn!("Node {} 序列号重复：{}", node_id, sequence);
                    Err(SequenceError::Duplicate)
                } else {
                    // 序列号回退（乱序）
                    tracing::warn!(
                        "Node {} 序列号回退：从 {} 到 {}（乱序）",
                        node_id, last, sequence
                    );
                    Err(SequenceError::OutOfOrder {
                        expected: last + 1,
                        actual: sequence,
                    })
                }
            }
        }
    }

    /// 重置 Node 的序列号状态（Node 重连时）
    pub fn reset(&mut self, node_id: &NodeId) {
        self.last_received.remove(node_id);
    }

    /// 获取 Node 的最后序列号
    pub fn get_last_sequence(&self, node_id: &NodeId) -> Option<u64> {
        self.last_received.get(node_id).copied()
    }
}

/// 序列号检查结果
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceCheckResult {
    /// 顺序正确（序列号连续）
    InOrder,
    /// 序列号跳跃（可能丢包）
    Gap(u64),
}

/// 序列号错误
#[derive(Debug, Clone, PartialEq)]
pub enum SequenceError {
    /// 重复消息
    Duplicate,
    /// 序列号乱序
    OutOfOrder {
        expected: u64,
        actual: u64,
    },
}

impl std::fmt::Display for SequenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Duplicate => write!(f, "重复消息"),
            Self::OutOfOrder { expected, actual } => {
                write!(f, "序列号乱序：期望 {}，实际 {}", expected, actual)
            }
        }
    }
}

impl std::error::Error for SequenceError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sequence_manager() {
        let mgr = SequenceManager::new("test".parse::<NodeId>().unwrap());
        
        assert_eq!(mgr.next_sequence(), 0);
        assert_eq!(mgr.next_sequence(), 1);
        assert_eq!(mgr.next_sequence(), 2);
        assert_eq!(mgr.current_sequence(), 3);
    }

    #[test]
    fn test_sequence_checker_in_order() {
        let mut checker = SequenceChecker::new(10);
        let node_id: NodeId = "node-1".parse().unwrap();
        
        // 正常顺序
        assert_eq!(
            checker.check(&node_id, 0).unwrap(),
            SequenceCheckResult::InOrder
        );
        assert_eq!(
            checker.check(&node_id, 1).unwrap(),
            SequenceCheckResult::InOrder
        );
        assert_eq!(
            checker.check(&node_id, 2).unwrap(),
            SequenceCheckResult::InOrder
        );
    }

    #[test]
    fn test_sequence_checker_gap() {
        let mut checker = SequenceChecker::new(10);
        let node_id: NodeId = "node-1".parse().unwrap();
        
        // 序列号跳跃（可能丢包）
        checker.check(&node_id, 0).unwrap();
        let result = checker.check(&node_id, 5).unwrap();
        assert_eq!(result, SequenceCheckResult::Gap(5));
    }

    #[test]
    fn test_sequence_checker_out_of_order() {
        let mut checker = SequenceChecker::new(10);
        let node_id: NodeId = "node-1".parse().unwrap();
        
        // 序列号回退（乱序）
        checker.check(&node_id, 5).unwrap();
        let result = checker.check(&node_id, 3);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            SequenceError::OutOfOrder {
                expected: 6,
                actual: 3
            }
        );
    }

    #[test]
    fn test_sequence_checker_duplicate() {
        let mut checker = SequenceChecker::new(10);
        let node_id: NodeId = "node-1".parse().unwrap();
        
        // 重复消息
        checker.check(&node_id, 5).unwrap();
        let result = checker.check(&node_id, 5);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), SequenceError::Duplicate);
    }
}