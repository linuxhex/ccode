//! ZeroMQ 消息路由器
//!
//! ROS 1 风格双面架构：
//! - 控制面：Kernel ROUTER/PUB 处理注册、发现、心跳
//! - 数据面：Node PUB/SUB 点对点传输业务数据
//!
//! Broker 职责（ROS 1 的 roscore 中的 Master 部分）：
//! - 维护 NodeId → ZMQ identity 映射（用于控制面 ROUTER 定向发送）
//! - 维护 topic pattern → NodeId 订阅关系
//! - 维护 topic → publisher PUB 地址映射（用于数据面发现）
//! - 维护 service → provider REP 地址映射（用于 Service 发现）
//!
//! 注意：Broker **不再转发业务数据**，数据面由 Node 间 PUB/SUB 直连。

use anyhow::Result;
use std::collections::HashMap;
use crate::message::frame::FrameCodec;
use crate::message::topic::topic_matches;
use crate::message::Message;
use crate::node::NodeId;

/// Broker 消息路由表项：每个 Node 的 REQ 连接 identity
type NodeIdentity = Vec<u8>;

/// Publisher 信息：Node 的数据面 PUB socket 地址
#[derive(Debug, Clone)]
pub struct PublisherInfo {
    /// Node ID
    pub node_id: NodeId,
    /// 该 Node 的 PUB socket 绑定地址（其他 Node 直连 SUB）
    pub pub_addr: String,
    /// 该 Node 发布的 topic 列表
    pub topics: Vec<String>,
}

/// Service Provider 信息：Node 的数据面 REP socket 地址
#[derive(Debug, Clone)]
pub struct ServiceProviderInfo {
    /// Node ID
    pub node_id: NodeId,
    /// 该 Node 的 REP socket 绑定地址（Client 直连 REQ）
    pub rep_addr: String,
    /// 该 Node 提供的 Service 名称
    pub service_name: String,
}

/// Broker - 消息总线核心路由器（ROS 1 Master 角色）
///
/// 纯路由逻辑，不持有 ZMQ socket。
/// 由 Kernel 在事件循环中调用路由方法。
///
/// ROS 1 风格关键变化：
/// - 业务数据不经过 Broker，Node 间 PUB/SUB 直连
/// - Broker 只维护发现信息（publisher 地址、service 地址）
/// - 新 Node 注册时，Broker 返回所需 topic 的 publisher 地址
pub struct Broker {
    /// Node ID → ZMQ identity 映射（控制面 ROUTER 定向发送用）
    node_identities: HashMap<NodeId, NodeIdentity>,
    /// topic pattern → 订阅了此 pattern 的 Node ID 列表（控制面用）
    subscriptions: HashMap<String, Vec<NodeId>>,
    /// Node ID → Publisher 信息（数据面发现）
    publishers: HashMap<NodeId, PublisherInfo>,
    /// topic → 发布该 topic 的 Node ID 列表（数据面发现）
    topic_publishers: HashMap<String, Vec<NodeId>>,
    /// service_name → Service Provider 信息（数据面发现）
    service_providers: HashMap<String, ServiceProviderInfo>,
}

impl Broker {
    pub fn new(_router_addr: String, _pub_addr: String) -> Self {
        Self {
            node_identities: HashMap::new(),
            subscriptions: HashMap::new(),
            publishers: HashMap::new(),
            topic_publishers: HashMap::new(),
            service_providers: HashMap::new(),
        }
    }

    // ---- 控制面：注册/注销 ----

    /// 注册 Node 的 ZMQ identity（控制面）
    pub fn register_identity(&mut self, node_id: NodeId, identity: NodeIdentity) {
        tracing::trace!(
            target: "ccore::bus",
            topic = "identity_register",
            size = identity.len(),
            "message published"
        );
        self.node_identities.insert(node_id, identity);
    }

    /// 注销 Node 的 ZMQ identity（控制面 + 数据面清理）
    pub fn deregister_identity(&mut self, node_id: &NodeId) {
        tracing::warn!(
            target: "ccore::bus",
            subscriber = %node_id,
            "subscriber disconnected"
        );
        self.node_identities.remove(node_id);
        // 清理该 Node 的所有订阅
        for subscribers in self.subscriptions.values_mut() {
            subscribers.retain(|id| id != node_id);
        }
        // 清理该 Node 的 publisher 信息
        if let Some(pub_info) = self.publishers.remove(node_id) {
            for topic in &pub_info.topics {
                if let Some(pubs) = self.topic_publishers.get_mut(topic) {
                    pubs.retain(|id| id != node_id);
                }
            }
        }
        // 清理该 Node 的 service provider 信息
        self.service_providers.retain(|_, v| &v.node_id != node_id);
    }

    /// 注册订阅关系（控制面）
    pub fn subscribe(&mut self, node_id: NodeId, topic_pattern: String) {
        let subscribers = self.subscriptions
            .entry(topic_pattern)
            .or_default();
        // 防止重复订阅
        if !subscribers.contains(&node_id) {
            subscribers.push(node_id);
        }
    }

    /// 取消订阅
    pub fn unsubscribe(&mut self, node_id: &NodeId, topic_pattern: &str) {
        if let Some(subscribers) = self.subscriptions.get_mut(topic_pattern) {
            subscribers.retain(|id| id != node_id);
        }
    }

    // ---- 数据面：Publisher 发现（ROS 1 核心能力）----

    /// 注册 Node 的 Publisher 信息
    ///
    /// 如果该 Node 已注册过 Publisher，先清理旧的 topic→node_id 映射，
    /// 再注册新的映射，确保重新注册时数据一致。
    pub fn register_publisher(&mut self, info: PublisherInfo) {
        let node_id = info.node_id.clone();
        let topics = info.topics.clone();

        // 清理该 Node 旧的 topic_publishers 映射（防止重复注册时出现脏数据）
        if let Some(old_info) = self.publishers.get(&node_id) {
            for old_topic in &old_info.topics {
                if let Some(pubs) = self.topic_publishers.get_mut(old_topic) {
                    pubs.retain(|id| id != &node_id);
                }
            }
        }

        // 注册新的 topic→node_id 映射
        for topic in &topics {
            self.topic_publishers
                .entry(topic.clone())
                .or_default()
                .push(node_id.clone());
        }
        self.publishers.insert(node_id, info);
    }

    /// 查找发布指定 topic 的所有 Publisher
    ///
    /// 同时匹配精确 topic 和通配符 pattern：
    /// - 精确匹配：topic_publishers[key=topic]
    /// - 通配符匹配：遍历 topic_publishers 中含 * 的 key，用 topic_matches 反向匹配
    ///
    /// 这样 ToolNode 注册 `agent/*/tool_result` 后，
    /// 查找 `agent/abc/tool_result` 也能找到它。
    pub fn find_publishers(&self, topic: &str) -> Vec<&PublisherInfo> {
        let mut result = Vec::new();

        // 精确匹配
        if let Some(publisher_ids) = self.topic_publishers.get(topic) {
            for id in publisher_ids {
                if let Some(info) = self.publishers.get(id) {
                    result.push(info);
                }
            }
        }

        // 通配符匹配：publisher 注册了含 * 的 pattern，检查 topic 是否匹配
        for (pattern, publisher_ids) in &self.topic_publishers {
            if pattern.contains('*') && topic_matches(pattern, topic) {
                for id in publisher_ids {
                    if let Some(info) = self.publishers.get(id) {
                        // 去重
                        if !result.iter().any(|r| r.node_id == info.node_id) {
                            result.push(info);
                        }
                    }
                }
            }
        }

        result
    }

    /// 查找新 Node 订阅的 topic 对应的所有 publisher
    ///
    /// 优化：直接使用 topic_publishers 索引查找，避免双重遍历。
    /// 对于通配符订阅（含 * 的 pattern），仍需遍历匹配。
    pub fn find_publishers_for_subscriptions(&self, subscriptions: &[String]) -> Vec<(String, Vec<PublisherInfo>)> {
        let mut result = Vec::with_capacity(subscriptions.len());

        for pattern in subscriptions {
            let mut matching_publishers = Vec::new();

            if pattern.contains('*') {
                // 通配符订阅：需要遍历所有 topic 匹配
                for (topic, publisher_ids) in &self.topic_publishers {
                    if topic_matches(pattern, topic) {
                        for id in publisher_ids {
                            if let Some(info) = self.publishers.get(id) {
                                matching_publishers.push(info.clone());
                            }
                        }
                    }
                }
            } else {
                // 精确订阅：直接查索引 O(1)
                if let Some(publisher_ids) = self.topic_publishers.get(pattern) {
                    for id in publisher_ids {
                        if let Some(info) = self.publishers.get(id) {
                            matching_publishers.push(info.clone());
                        }
                    }
                }
            }

            if !matching_publishers.is_empty() {
                matching_publishers.sort_by(|a, b| a.node_id.to_string().cmp(&b.node_id.to_string()));
                matching_publishers.dedup_by(|a, b| a.node_id == b.node_id);
                result.push((pattern.clone(), matching_publishers));
            }
        }
        result
    }

    // ---- 数据面：Service 发现 ----

    /// 注册 Service Provider
    pub fn register_service_provider(&mut self, info: ServiceProviderInfo) {
        self.service_providers.insert(info.service_name.clone(), info);
    }

    /// 查找 Service Provider
    pub fn find_service_provider(&self, service_name: &str) -> Option<&ServiceProviderInfo> {
        self.service_providers.get(service_name)
    }

    // ---- 控制面：辅助方法 ----

    /// 查找订阅了指定 topic 的所有 Node
    pub fn find_subscribers(&self, topic: &str) -> Vec<NodeId> {
        let mut result = Vec::new();
        for (pattern, subscribers) in &self.subscriptions {
            if topic_matches(pattern, topic) {
                result.extend(subscribers.iter().cloned());
            }
        }
        result.sort_by_key(|a| a.to_string());
        result.dedup();
        result
    }

    /// 查找可能对新 Publisher 感兴趣的订阅者
    ///
    /// 给定一个新 Publisher 发布的 topic 列表，找出订阅了这些 topic 的 Node。
    /// 用于新 Publisher 上线时通知已有订阅者建立数据面 SUB 连接。
    pub fn find_subscribers_for_publisher(&self, publisher_topics: &[String]) -> Vec<NodeId> {
        let mut result = Vec::new();
        for topic in publisher_topics {
            for (pattern, subscribers) in &self.subscriptions {
                if topic_matches(pattern, topic) {
                    result.extend(subscribers.iter().cloned());
                }
            }
        }
        result.sort_by_key(|a| a.to_string());
        result.dedup();
        result
    }

    /// 获取 Node 的 ZMQ identity
    pub fn get_identity(&self, node_id: &NodeId) -> Option<&NodeIdentity> {
        self.node_identities.get(node_id)
    }

    /// 查找控制面消息的目标（仅用于系统消息路由）
    pub fn find_targets(&self, msg: &Message) -> Vec<(NodeIdentity, NodeId)> {
        let topic = msg.topic.as_str();
        let subscribers = self.find_subscribers(topic);
        subscribers
            .iter()
            .filter(|id| id.as_str() != msg.header.src_node)
            .filter_map(|id| {
                let identity = self.node_identities.get(id)?;
                Some((identity.clone(), id.clone()))
            })
            .collect()
    }

    /// 路由控制面消息（仅用于系统消息）
    pub fn route_message(&self, msg: &Message) -> Result<Vec<(NodeIdentity, Vec<Vec<u8>>)>> {
        let topic = msg.topic.as_str();
        let subscribers = self.find_subscribers(topic);
        let frames = FrameCodec::encode(msg)?;
        tracing::trace!(
            target: "ccore::bus",
            topic = %topic,
            size = frames.iter().map(|f| f.len()).sum::<usize>(),
            "message published"
        );
        let targets: Vec<(NodeIdentity, Vec<Vec<u8>>)> = subscribers
            .iter()
            .filter(|id| id.as_str() != msg.header.src_node)
            .filter_map(|id| {
                let identity = self.node_identities.get(id)?;
                Some((identity.clone(), frames.clone()))
            })
            .collect();
        Ok(targets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Topic;

    #[test]
    fn test_subscribe_and_route() {
        let mut broker = Broker::new("ipc:///tmp/test".into(), "ipc:///tmp/test-pub".into());

        let agent_id: NodeId = "agent-1".parse().unwrap();
        let tool_id: NodeId = "tool-1".parse().unwrap();

        // 注册 identity
        broker.register_identity(agent_id.clone(), b"agent-1-identity".to_vec());
        broker.register_identity(tool_id.clone(), b"tool-1-identity".to_vec());

        // Agent 订阅自己的消息
        broker.subscribe(agent_id.clone(), format!("agent/{}/input", agent_id));
        broker.subscribe(agent_id.clone(), format!("agent/{}/tool_result", agent_id));

        // Tool 订阅所有 Agent 的工具调用
        broker.subscribe(tool_id.clone(), "agent/*/tool_call".into());

        // 模拟 Agent 发送 tool_call
        let msg = FrameCodec::new_message(
            Topic::agent_tool_call("agent-1"),
            "agent-1",
            &serde_json::json!({"tool_name": "bash", "args": "ls"}),
        ).unwrap();

        let targets = broker.route_message(&msg).unwrap();

        // 应该只路由到 Tool Node（不回发给自己）
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].0, b"tool-1-identity".to_vec());
    }

    #[test]
    fn test_wildcard_matching() {
        let mut broker = Broker::new("ipc:///tmp/test".into(), "ipc:///tmp/test-pub".into());

        let tui_id: NodeId = "tui-1".parse().unwrap();
        broker.register_identity(tui_id.clone(), b"tui-1-identity".to_vec());

        // TUI 订阅所有 Agent 的输出
        broker.subscribe(tui_id.clone(), "agent/*/output".into());

        let subscribers = broker.find_subscribers("agent/abc/output");
        assert_eq!(subscribers.len(), 1);
        assert_eq!(subscribers[0], tui_id);
    }
}
