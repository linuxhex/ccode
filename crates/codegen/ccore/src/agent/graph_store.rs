//! Sub-agent graph store — 轻量级内存图，追踪代理父子关系
//!
//! 借鉴 Codex V2 的 graph store + LRU residency 模式。提供：
//! - 节点注册（agent_id, parent_id, type, status）
//! - 树查询（children, descendants, ancestors）
//! - LRU 驻留追踪（有界活跃集）
//! - 可选序列化（会话恢复）

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};

/// 代理在图中的状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// 已创建但未启动
    Pending,
    /// 正在运行
    Running,
    /// 已完成
    Completed,
    /// 已崩溃/失败
    Failed,
    /// 已取消
    Cancelled,
}

/// 图中的代理节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNode {
    pub agent_id: String,
    pub parent_id: Option<String>,
    pub agent_type: String,
    pub status: AgentStatus,
    pub spawn_depth: u32,
    pub description: Option<String>,
    /// 创建时间（Unix 毫秒）
    pub created_at_ms: i64,
    /// 完成时间（Unix 毫秒）
    pub finished_at_ms: Option<i64>,
}

/// 子代理图存储
///
/// 维护代理间的父子关系，支持树查询和 LRU 驻留追踪。
/// 线程安全由调用方保证（通常在单线程 actor 中使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStore {
    nodes: HashMap<String, AgentNode>,
    children: HashMap<String, Vec<String>>,
    lru_order: VecDeque<String>,
    max_resident: usize,
}

impl Default for GraphStore {
    fn default() -> Self {
        Self::new(64)
    }
}

impl GraphStore {
    pub fn new(max_resident: usize) -> Self {
        Self {
            nodes: HashMap::new(),
            children: HashMap::new(),
            lru_order: VecDeque::new(),
            max_resident,
        }
    }

    /// 注册一个新代理节点。返回 false 如果已存在。
    pub fn register(&mut self, node: AgentNode) -> bool {
        let id = node.agent_id.clone();
        if self.nodes.contains_key(&id) {
            return false;
        }

        if let Some(ref parent) = node.parent_id {
            self.children.entry(parent.clone()).or_default().push(id.clone());
        }

        self.nodes.insert(id.clone(), node);
        self.touch(&id);
        true
    }

    /// 更新代理状态
    pub fn set_status(&mut self, agent_id: &str, status: AgentStatus) {
        if let Some(node) = self.nodes.get_mut(agent_id) {
            node.status = status;
            if matches!(status, AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled) {
                node.finished_at_ms = Some(now_ms());
            }
            self.touch(agent_id);
        }
    }

    /// 获取节点
    pub fn get(&self, agent_id: &str) -> Option<&AgentNode> {
        self.nodes.get(agent_id)
    }

    /// 获取直接子代理
    pub fn children(&self, agent_id: &str) -> &[String] {
        self.children.get(agent_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 获取所有后代（BFS）
    pub fn descendants(&self, agent_id: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(agent_id.to_string());

        while let Some(current) = queue.pop_front() {
            if let Some(kids) = self.children.get(&current) {
                for child in kids {
                    result.push(child.clone());
                    queue.push_back(child.clone());
                }
            }
        }
        result
    }

    /// 获取祖先链（从父到根）
    pub fn ancestors(&self, agent_id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = self.nodes.get(agent_id).and_then(|n| n.parent_id.clone());
        while let Some(id) = current {
            chain.push(id.clone());
            current = self.nodes.get(&id).and_then(|n| n.parent_id.clone());
        }
        chain
    }

    /// 获取所有活跃（Running）代理
    pub fn active_agents(&self) -> Vec<&AgentNode> {
        self.nodes.values().filter(|n| n.status == AgentStatus::Running).collect()
    }

    /// 获取代理的深度（到根的距离）
    pub fn depth(&self, agent_id: &str) -> u32 {
        self.nodes.get(agent_id).map(|n| n.spawn_depth).unwrap_or(0)
    }

    /// 总节点数
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// LRU touch — 标记为最近使用，驱逐超出上限的节点
    fn touch(&mut self, agent_id: &str) {
        self.lru_order.retain(|id| id != agent_id);
        self.lru_order.push_front(agent_id.to_string());

        while self.lru_order.len() > self.max_resident {
            if let Some(evicted) = self.lru_order.pop_back() {
                // Only evict terminal nodes; running nodes stay resident.
                let should_evict = self.nodes.get(&evicted)
                    .map(|n| matches!(n.status, AgentStatus::Completed | AgentStatus::Failed | AgentStatus::Cancelled))
                    .unwrap_or(true);
                if should_evict {
                    self.remove_node(&evicted);
                } else {
                    // Put it back at the front; can't evict running nodes.
                    self.lru_order.push_front(evicted);
                    break;
                }
            }
        }
    }

    fn remove_node(&mut self, agent_id: &str) {
        if let Some(node) = self.nodes.remove(agent_id) {
            if let Some(ref parent) = node.parent_id {
                if let Some(kids) = self.children.get_mut(parent) {
                    kids.retain(|id| id != agent_id);
                }
            }
            // Orphan children (keep them but clear parent link in the children map)
            self.children.remove(agent_id);
        }
    }

    /// 序列化为 JSON（用于会话持久化）
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// 从 JSON 恢复
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, parent: Option<&str>, depth: u32) -> AgentNode {
        AgentNode {
            agent_id: id.to_string(),
            parent_id: parent.map(|s| s.to_string()),
            agent_type: "explore".to_string(),
            status: AgentStatus::Running,
            spawn_depth: depth,
            description: None,
            created_at_ms: 0,
            finished_at_ms: None,
        }
    }

    #[test]
    fn register_and_query() {
        let mut store = GraphStore::new(64);
        assert!(store.register(node("root", None, 0)));
        assert!(store.register(node("child1", Some("root"), 1)));
        assert!(store.register(node("child2", Some("root"), 1)));
        assert!(store.register(node("grandchild", Some("child1"), 2)));

        assert_eq!(store.len(), 4);
        assert_eq!(store.children("root"), &["child1", "child2"]);
        assert_eq!(store.children("child1"), &["grandchild"]);
        assert!(store.children("grandchild").is_empty());
    }

    #[test]
    fn descendants_bfs() {
        let mut store = GraphStore::new(64);
        store.register(node("a", None, 0));
        store.register(node("b", Some("a"), 1));
        store.register(node("c", Some("a"), 1));
        store.register(node("d", Some("b"), 2));
        store.register(node("e", Some("b"), 2));

        let desc = store.descendants("a");
        assert_eq!(desc.len(), 4);
        assert!(desc.contains(&"b".to_string()));
        assert!(desc.contains(&"c".to_string()));
        assert!(desc.contains(&"d".to_string()));
        assert!(desc.contains(&"e".to_string()));
    }

    #[test]
    fn ancestors_chain() {
        let mut store = GraphStore::new(64);
        store.register(node("a", None, 0));
        store.register(node("b", Some("a"), 1));
        store.register(node("c", Some("b"), 2));

        let chain = store.ancestors("c");
        assert_eq!(chain, vec!["b", "a"]);
    }

    #[test]
    fn status_update() {
        let mut store = GraphStore::new(64);
        store.register(node("a", None, 0));
        assert_eq!(store.get("a").unwrap().status, AgentStatus::Running);

        store.set_status("a", AgentStatus::Completed);
        assert_eq!(store.get("a").unwrap().status, AgentStatus::Completed);
        assert!(store.get("a").unwrap().finished_at_ms.is_some());
    }

    #[test]
    fn active_agents_filter() {
        let mut store = GraphStore::new(64);
        store.register(node("a", None, 0));
        store.register(node("b", Some("a"), 1));
        store.register(node("c", Some("a"), 1));

        store.set_status("b", AgentStatus::Completed);
        let active = store.active_agents();
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn lru_eviction() {
        let mut store = GraphStore::new(3);
        store.register(node("a", None, 0));
        store.set_status("a", AgentStatus::Completed);
        store.register(node("b", Some("a"), 1));
        store.set_status("b", AgentStatus::Completed);
        store.register(node("c", Some("a"), 1));
        store.set_status("c", AgentStatus::Completed);

        // Adding a 4th should evict the oldest completed (a)
        store.register(node("d", Some("a"), 1));
        assert_eq!(store.len(), 3);
        assert!(store.get("a").is_none());
    }

    #[test]
    fn lru_does_not_evict_running() {
        let mut store = GraphStore::new(2);
        store.register(node("a", None, 0));
        // "a" is Running, should not be evicted
        store.register(node("b", Some("a"), 1));
        store.set_status("b", AgentStatus::Completed);
        store.register(node("c", Some("a"), 1));
        store.set_status("c", AgentStatus::Completed);

        // "a" must still be present despite being oldest
        assert!(store.get("a").is_some());
    }

    #[test]
    fn duplicate_register_rejected() {
        let mut store = GraphStore::new(64);
        assert!(store.register(node("a", None, 0)));
        assert!(!store.register(node("a", None, 0)));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn serialization_roundtrip() {
        let mut store = GraphStore::new(64);
        store.register(node("root", None, 0));
        store.register(node("child", Some("root"), 1));
        store.set_status("child", AgentStatus::Completed);

        let json = store.to_json().unwrap();
        let restored = GraphStore::from_json(&json).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored.get("child").unwrap().status, AgentStatus::Completed);
        assert_eq!(restored.children("root"), &["child"]);
    }

    #[test]
    fn depth_query() {
        let mut store = GraphStore::new(64);
        store.register(node("a", None, 0));
        store.register(node("b", Some("a"), 1));
        store.register(node("c", Some("b"), 2));

        assert_eq!(store.depth("a"), 0);
        assert_eq!(store.depth("b"), 1);
        assert_eq!(store.depth("c"), 2);
        assert_eq!(store.depth("nonexistent"), 0);
    }
}
