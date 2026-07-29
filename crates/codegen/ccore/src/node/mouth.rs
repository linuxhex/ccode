//! Mouth Node — 输出器官（嘴巴）
//!
//! 仿生架构中的"嘴巴"，负责将思维转化为输出：
//! - 接收 ThinkerNode 的说话指令（cortex/{agent_id}/speak）
//! - 格式化输出内容，发布到 agent/{agent_id}/output（兼容现有 TUI 订阅）
//! - 生成状态报告
//!
//! Mouth 是仿生系统的输出通道，类似人类嘴巴将思维表达为语言。

use async_trait::async_trait;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// Mouth Node 实现
pub struct MouthNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 关联的 Agent ID（用于接收 cortex/{agent_id}/speak 和发布 agent/{agent_id}/output）
    agent_id: Option<String>,
}

impl MouthNode {
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
impl Node for MouthNode {
    fn node_type(&self) -> NodeType {
        NodeType::Mouth
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Mouth Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        // 匹配 cortex/{agent_id}/speak
        if topic.starts_with("cortex/") && topic.ends_with("/speak") {
            let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
            let content = payload["content"].as_str().unwrap_or("");
            let channel = payload["channel"].as_str().unwrap_or("text");

            // 格式化输出
            let formatted = match channel {
                "code" => format!("```\n{}\n```", content),
                "markdown" => content.to_string(),
                _ => content.to_string(),
            };

            // 发布到 agent/{agent_id}/output（兼容现有 TUI 订阅）
            if let Some(ref agent_id) = self.agent_id {
                let output_msg = FrameCodec::new_message(
                    Topic::agent_output(agent_id),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": channel,
                        "content": formatted,
                        "source": "mouth",
                    }),
                )?;
                transport.send_message(&output_msg).await?;
                tracing::debug!("Mouth {} 输出内容到 agent/{}/output", self.id, agent_id);
            } else {
                tracing::warn!("Mouth {} 收到说话指令但未设置 agent_id，无法输出", self.id);
            }
            return Ok(());
        }

        match topic {
            "mouth/status" => {
                // 生成状态报告
                let status = serde_json::json!({
                    "node_id": self.id.as_str(),
                    "node_type": "mouth",
                    "agent_id": self.agent_id,
                    "status": "active",
                });
                let status_msg = FrameCodec::new_message(
                    Topic::agent_output(self.agent_id.as_deref().unwrap_or("unknown")),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "status",
                        "content": status.to_string(),
                    }),
                )?;
                transport.send_message(&status_msg).await?;
            }

            "sys/shutdown" => {
                tracing::info!("Mouth Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Mouth Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subs = vec![
            "mouth/status".into(),
            "sys/shutdown".into(),
        ];
        if let Some(ref agent_id) = self.agent_id {
            subs.push(format!("cortex/{}/speak", agent_id));
        }
        subs
    }

    fn published_topics(&self) -> Vec<String> {
        match &self.agent_id {
            Some(agent_id) => vec![format!("agent/{}/output", agent_id)],
            None => vec![],
        }
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Mouth Node 关闭：{}", self.id);
        Ok(())
    }
}
