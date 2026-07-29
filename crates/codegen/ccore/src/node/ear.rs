//! Ear Node — 听觉器官
//!
//! 仿生架构中的"耳朵"，负责接收外部声音：
//! - 接收用户输入（自然语言指令）
//! - 接收系统通知
//! - 转发到 ThinkerNode（cortex/{agent_id}/input）进行思考
//!
//! Ear 是信息进入仿生系统的主入口，类似人类听觉将声音传入大脑。

use async_trait::async_trait;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// Ear Node 实现
pub struct EarNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 关联的 Agent ID（用于转发到 cortex/{agent_id}/input）
    agent_id: Option<String>,
}

impl EarNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            agent_id: None,
        }
    }

    /// 设置关联的 Agent ID
    pub fn set_agent_id(&mut self, agent_id: String) {
        self.agent_id = Some(agent_id);
    }
}

#[async_trait]
impl Node for EarNode {
    fn node_type(&self) -> NodeType {
        NodeType::Ear
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Ear Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            "ear/hear" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let content = payload["content"].as_str().unwrap_or("");
                let role = payload["role"].as_str().unwrap_or("user");

                // 转发到 cortex/{agent_id}/input
                if let Some(ref agent_id) = self.agent_id {
                    let forward_msg = FrameCodec::new_message(
                        Topic::cortex_input(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "content": content,
                            "role": role,
                            "source": "ear",
                        }),
                    )?;
                    transport.send_message(&forward_msg).await?;
                    tracing::debug!("Ear {} 转发用户输入到 cortex/{}", self.id, agent_id);
                } else {
                    tracing::warn!("Ear {} 收到用户输入但未设置 agent_id，无法转发", self.id);
                }
            }

            "ear/notification" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let notification = payload["message"].as_str().unwrap_or("");
                let level = payload["level"].as_str().unwrap_or("info");

                // 转发系统通知到 cortex
                if let Some(ref agent_id) = self.agent_id {
                    let notify_msg = FrameCodec::new_message(
                        Topic::cortex_input(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "content": format!("[系统通知][{}] {}", level, notification),
                            "role": "system",
                            "source": "ear/notification",
                        }),
                    )?;
                    transport.send_message(&notify_msg).await?;
                } else {
                    tracing::warn!("Ear {} 收到系统通知但未设置 agent_id，无法转发", self.id);
                }
            }

            "sys/shutdown" => {
                tracing::info!("Ear Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Ear Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "ear/hear".into(),
            "ear/notification".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        match &self.agent_id {
            Some(agent_id) => vec![format!("cortex/{}/input", agent_id)],
            None => vec![],
        }
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Ear Node 关闭：{}", self.id);
        Ok(())
    }
}
