//! Kernel 状态事务管理
//!
//! 确保 Registry 和 Broker 的状态更新的原子性。
//!
//! 设计：
//! - 使用事务模式：Prepare -> Commit -> Rollback
//! - 记录操作日志，失败时可回滚
//! - 确保状态一致性，避免部分成功

use anyhow::Result;

use crate::kernel::broker::Broker;
use crate::kernel::registry::Registry;
use crate::node::{NodeId, NodeType};

/// Kernel 状态事务管理器
pub struct KernelTransaction<'a> {
    broker: &'a mut Broker,
    registry: &'a mut Registry,
    /// 操作日志（用于回滚）
    operations: Vec<Operation>,
}

/// 操作记录
///
/// 记录注册/注销操作，用于未来实现回滚逻辑。
/// 当前回滚功能未实现，因为底层 register_identity 不返回错误，
/// 注册失败场景下 Node 会自动重连。
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Operation {
    RegisterNode {
        node_id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    },
    DeregisterNode {
        node_id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    },
    Subscribe {
        node_id: NodeId,
        topic_pattern: String,
    },
}

impl<'a> KernelTransaction<'a> {
    pub fn new(broker: &'a mut Broker, registry: &'a mut Registry) -> Self {
        Self {
            broker,
            registry,
            operations: Vec::new(),
        }
    }

    /// 原子注册 Node
    ///
    /// 步骤：
    /// 1. Registry 注册（成功继续，失败返回错误）
    /// 2. Broker 注册 identity（成功继续，失败回滚 Registry）
    /// 3. Broker 订阅 topics（逐个添加，失败回滚所有）
    ///
    /// 注意：当前 Registry/Broker 的注册操作不返回错误（内存操作不会失败），
    /// 因此 rollback 目前不会被触发。但保留事务模式是为了：
    /// - 未来 Registry/Broker 可能引入持久化（如磁盘 I/O），可能失败
    /// - 防止新增操作时遗漏回滚逻辑
    pub fn register_node(
        &mut self,
        node_id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    ) -> Result<()> {
        // 步骤 1: Registry 注册
        self.registry.register(node_id.clone(), node_type, subscriptions.clone());
        
        // 记录操作（用于回滚）
        self.operations.push(Operation::RegisterNode {
            node_id: node_id.clone(),
            node_type,
            subscriptions: subscriptions.clone(),
            identity: identity.clone(),
        });

        // 步骤 2: Broker 注册 identity
        self.broker.register_identity(node_id.clone(), identity.clone());

        // 步骤 3: Broker 订阅 topics
        for pattern in &subscriptions {
            self.broker.subscribe(node_id.clone(), pattern.clone());
            self.operations.push(Operation::Subscribe {
                node_id: node_id.clone(),
                topic_pattern: pattern.clone(),
            });
        }

        Ok(())
    }

    /// 原子注销 Node
    ///
    /// 步骤：
    /// 1. Registry 获取 Node 信息（用于回滚）
    /// 2. Registry 注销
    /// 3. Broker 注销 identity 和 subscriptions
    pub fn deregister_node(&mut self, node_id: &NodeId) -> Result<()> {
        // 步骤 1: 获取 Node 信息（用于回滚）
        let node_info = self.registry.get(node_id);
        
        if let Some(info) = node_info {
            // 记录操作（用于回滚）
            self.operations.push(Operation::DeregisterNode {
                node_id: node_id.clone(),
                node_type: info.node_type,
                subscriptions: info.subscriptions.clone(),
                identity: Vec::new(), // identity 无法从 broker 获取，但注销时不需要
            });

            // 步骤 2: Registry 注销
            self.registry.deregister(node_id);

            // 步骤 3: Broker 注销
            self.broker.deregister_identity(node_id);
        }

        Ok(())
    }

    /// 提交事务（清理操作日志）
    pub fn commit(self) {
        // 操作已执行，清理日志
        tracing::debug!("Kernel 事务提交，操作数：{}", self.operations.len());
    }

    /// 回滚事务（撤销所有操作）
    pub fn rollback(self) {
        tracing::warn!("Kernel 事务回滚，撤销 {} 个操作", self.operations.len());

        // 反向执行操作，撤销更改
        for op in self.operations.into_iter().rev() {
            match op {
                Operation::RegisterNode { node_id, .. } => {
                    // 撤销注册
                    self.registry.deregister(&node_id);
                    self.broker.deregister_identity(&node_id);
                }
                Operation::DeregisterNode {
                    node_id,
                    node_type,
                    subscriptions,
                    identity,
                } => {
                    // 恢复注册
                    self.registry.register(node_id.clone(), node_type, subscriptions);
                    self.broker.register_identity(node_id, identity);
                }
                Operation::Subscribe {
                    node_id,
                    topic_pattern,
                } => {
                    // 撤销订阅
                    self.broker.unsubscribe(&node_id, &topic_pattern);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_node_commit() {
        let mut broker = Broker::new("test".into(), "test".into());
        let mut registry = Registry::new();
        
        let node_id: NodeId = "test-node".parse().unwrap();
        let mut tx = KernelTransaction::new(&mut broker, &mut registry);
        
        tx.register_node(
            node_id.clone(),
            NodeType::Agent,
            vec!["test/*".into()],
            b"identity".to_vec(),
        ).unwrap();
        
        tx.commit();
        
        // 验证注册成功
        assert!(registry.get(&node_id).is_some());
        assert!(broker.get_identity(&node_id).is_some());
    }

    #[test]
    fn test_register_node_rollback() {
        let mut broker = Broker::new("test".into(), "test".into());
        let mut registry = Registry::new();
        
        let node_id: NodeId = "test-node".parse().unwrap();
        let mut tx = KernelTransaction::new(&mut broker, &mut registry);
        
        tx.register_node(
            node_id.clone(),
            NodeType::Agent,
            vec!["test/*".into()],
            b"identity".to_vec(),
        ).unwrap();
        
        tx.rollback();
        
        // 验证注册已撤销
        assert!(registry.get(&node_id).is_none());
        assert!(broker.get_identity(&node_id).is_none());
    }
}