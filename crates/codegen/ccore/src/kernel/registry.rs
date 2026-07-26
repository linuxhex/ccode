//! Node 注册表 - 维护所有在线 Node 的信息

use std::collections::HashMap;
use chrono::Utc;
use crate::node::{NodeId, NodeType};

/// Node 注册信息
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub node_type: NodeType,
    pub subscriptions: Vec<String>,
    pub last_heartbeat: chrono::DateTime<Utc>,
}

/// Node 注册表
pub struct Registry {
    nodes: HashMap<NodeId, NodeInfo>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
        }
    }

    /// 注册新 Node
    pub fn register(&mut self, id: NodeId, node_type: NodeType, subscriptions: Vec<String>) {
        let info = NodeInfo {
            node_type,
            subscriptions,
            last_heartbeat: Utc::now(),
        };
        self.nodes.insert(id, info);
    }

    /// 注销 Node
    pub fn deregister(&mut self, id: &NodeId) {
        self.nodes.remove(id);
    }

    /// 更新心跳时间
    pub fn heartbeat(&mut self, id: &NodeId) {
        if let Some(info) = self.nodes.get_mut(id) {
            info.last_heartbeat = Utc::now();
        }
    }

    /// 移除超时未心跳的 Node，返回被移除的 Node ID 列表
    pub fn remove_stale(&mut self, timeout_secs: u64) -> Vec<NodeId> {
        let now = Utc::now();
        let stale: Vec<NodeId> = self.nodes
            .iter()
            .filter(|(_, info)| {
                let elapsed = now.signed_duration_since(info.last_heartbeat);
                elapsed.num_seconds() as u64 > timeout_secs
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &stale {
            self.nodes.remove(id);
        }
        stale
    }

    /// 查找订阅了指定 topic 的所有 Node
    pub fn find_subscribers(&self, topic: &str) -> Vec<&NodeId> {
        self.nodes
            .iter()
            .filter(|(_, info)| {
                info.subscriptions.iter().any(|sub| {
                    crate::message::topic::topic_matches(sub, topic)
                })
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// 按类型查找 Node
    pub fn find_by_type(&self, node_type: NodeType) -> Vec<&NodeId> {
        self.nodes
            .iter()
            .filter(|(_, info)| info.node_type == node_type)
            .map(|(id, _)| id)
            .collect()
    }

    /// 获取 Node 信息
    pub fn get(&self, id: &NodeId) -> Option<&NodeInfo> {
        self.nodes.get(id)
    }

    /// 在线 Node 数量
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}
