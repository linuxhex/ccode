//! ZeroMQ 消息路由器
//!
//! 纯路由逻辑层，与 ZMQ 传输解耦。
//!
//! 职责：
//! - 维护 NodeId → ZMQ identity 映射（用于 ROUTER 定向发送）
//! - 维护 topic pattern → NodeId 订阅关系
//! - 根据消息 topic 查找订阅者，返回 (identity, frames) 列表
//!
//! 实际的 ZMQ I/O 由 KernelTransport 负责，Broker 只做路由决策。

use anyhow::Result;
use std::collections::HashMap;
use crate::message::frame::FrameCodec;
use crate::message::topic::topic_matches;
use crate::message::Message;
use crate::node::NodeId;

/// Broker 消息路由表项：每个 Node 的 REQ 连接 identity
type NodeIdentity = Vec<u8>;

/// Broker - 消息总线核心路由器
///
/// 纯路由逻辑，不持有 ZMQ socket。
/// 由 Kernel 在事件循环中调用路由方法，再通过 KernelTransport 发送。
pub struct Broker {
    /// Node ID → ZMQ identity 映射，用于 REQ/REP 模式定向发送
    node_identities: HashMap<NodeId, NodeIdentity>,
    /// topic pattern → 订阅了此 pattern 的 Node ID 列表
    subscriptions: HashMap<String, Vec<NodeId>>,
}

/// Broker 事件，由事件循环产出
#[derive(Debug)]
pub enum BrokerEvent {
    /// 收到来自 Node 的消息
    Message {
        from: NodeId,
        message: Message,
    },
    /// Node 连接上线
    NodeConnected {
        id: NodeId,
        identity: NodeIdentity,
    },
    /// Node 连接断开
    NodeDisconnected {
        identity: NodeIdentity,
    },
}

/// Broker 命令，由外部注入事件循环
#[derive(Debug)]
pub enum BrokerCommand {
    /// 向指定 Node 发送消息
    Send {
        target: NodeId,
        message: Message,
    },
    /// 广播消息到所有订阅了指定 topic 的 Node
    Broadcast {
        topic: String,
        message: Message,
    },
    /// 注册 Node 的订阅关系
    Subscribe {
        node_id: NodeId,
        topic_pattern: String,
    },
    /// 取消 Node 的订阅
    Unsubscribe {
        node_id: NodeId,
        topic_pattern: String,
    },
    /// 关闭 Broker
    Shutdown,
}

impl Broker {
    pub fn new(_router_addr: String, _pub_addr: String) -> Self {
        Self {
            node_identities: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }

    // ---- 路由逻辑（纯逻辑，与 ZMQ 传输解耦）----

    /// 注册 Node 的 ZMQ identity
    pub fn register_identity(&mut self, node_id: NodeId, identity: NodeIdentity) {
        self.node_identities.insert(node_id, identity);
    }

    /// 注销 Node 的 ZMQ identity
    pub fn deregister_identity(&mut self, node_id: &NodeId) {
        self.node_identities.remove(node_id);
        // 同时清理该 Node 的所有订阅
        for subscribers in self.subscriptions.values_mut() {
            subscribers.retain(|id| id != node_id);
        }
    }

    /// 注册订阅关系
    pub fn subscribe(&mut self, node_id: NodeId, topic_pattern: String) {
        self.subscriptions
            .entry(topic_pattern)
            .or_default()
            .push(node_id);
    }

    /// 取消订阅
    pub fn unsubscribe(&mut self, node_id: &NodeId, topic_pattern: &str) {
        if let Some(subscribers) = self.subscriptions.get_mut(topic_pattern) {
            subscribers.retain(|id| id != node_id);
        }
    }

    /// 查找订阅了指定 topic 的所有 Node
    pub fn find_subscribers(&self, topic: &str) -> Vec<NodeId> {
        let mut result = Vec::new();
        for (pattern, subscribers) in &self.subscriptions {
            if topic_matches(pattern, topic) {
                result.extend(subscribers.iter().cloned());
            }
        }
        // 去重（一个 Node 可能匹配多个 pattern）
        result.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
        result.dedup();
        result
    }

    /// 获取 Node 的 ZMQ identity
    pub fn get_identity(&self, node_id: &NodeId) -> Option<&NodeIdentity> {
        self.node_identities.get(node_id)
    }

    /// 处理一条收到的消息：路由到目标订阅者
    ///
    /// 返回需要发送的 (target_identity, encoded_frames) 列表
    pub fn route_message(&self, msg: &Message) -> Result<Vec<(NodeIdentity, Vec<Vec<u8>>)>> {
        let topic = msg.topic.as_str();
        let subscribers = self.find_subscribers(topic);

        // 不回发给发送者自己（避免循环）
        let frames = FrameCodec::encode(msg)?;

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

        let agent_id = NodeId::from_str("agent-1");
        let tool_id = NodeId::from_str("tool-1");

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

        let tui_id = NodeId::from_str("tui-1");
        broker.register_identity(tui_id.clone(), b"tui-1-identity".to_vec());

        // TUI 订阅所有 Agent 的输出
        broker.subscribe(tui_id.clone(), "agent/*/output".into());

        let subscribers = broker.find_subscribers("agent/abc/output");
        assert_eq!(subscribers.len(), 1);
        assert_eq!(subscribers[0], tui_id);
    }
}
