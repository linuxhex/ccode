//! AcpNode — IDE/stdio ACP client 与消息总线之间的边界
//!
//! 职责：
//! - 把 ACP `session/prompt` 转为 `agent/{id}/input`
//! - 把 `agent/{id}/output` 流转为 ACP session updates
//! - 处理 `agent/{id}/permission` ↔ ACP permission request
//! - 发布 `agent/{id}/cancel`
//!
//! 实现来源：从 `ccode-shell` MvpAgent/ACP gateway 迁入，不在此 Node 内跑 agentic loop。

use async_trait::async_trait;

use crate::message::payloads::{CancelRequest, PermissionResponse};
use crate::message::{FrameCodec, Message, Topic};
use crate::node::transport::NodeTransportHandle;
use crate::node::{Node, NodeContext, NodeId, NodeType};

pub struct AcpNode {
    id: NodeId,
    primary_agent_id: String,
}

impl AcpNode {
    pub fn new(id: NodeId, primary_agent_id: impl Into<String>) -> Self {
        Self {
            id,
            primary_agent_id: primary_agent_id.into(),
        }
    }

    pub fn set_primary_agent(&mut self, agent_id: impl Into<String>) {
        self.primary_agent_id = agent_id.into();
    }

    /// ACP prompt → bus input
    async fn publish_user_input(
        &self,
        text: &str,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let topic = Topic::agent_input(&self.primary_agent_id);
        let msg = FrameCodec::new_message(
            topic,
            self.id.as_str(),
            &serde_json::json!({ "text": text }),
        )?;
        if let Err(e) = transport.publish_data(&msg).await {
            tracing::warn!("AcpNode data-plane publish failed: {}, falling back", e);
            transport.send_message(&msg).await?;
        }
        Ok(())
    }

    async fn publish_cancel(
        &self,
        reason: Option<String>,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let payload = CancelRequest {
            agent_id: self.primary_agent_id.clone(),
            reason,
        };
        let msg = FrameCodec::new_message(
            Topic::agent_cancel(&self.primary_agent_id),
            self.id.as_str(),
            &payload,
        )?;
        if let Err(e) = transport.publish_data(&msg).await {
            tracing::warn!("AcpNode data-plane publish failed: {}, falling back", e);
            transport.send_message(&msg).await?;
        }
        Ok(())
    }

    async fn publish_permission_response(
        &self,
        response: PermissionResponse,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let msg = FrameCodec::new_message(
            Topic::agent_permission(&response.agent_id),
            self.id.as_str(),
            &response,
        )?;
        if let Err(e) = transport.publish_data(&msg).await {
            tracing::warn!("AcpNode data-plane publish failed: {}, falling back", e);
            transport.send_message(&msg).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Node for AcpNode {
    fn node_type(&self) -> NodeType {
        NodeType::Acp
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(node_id = %self.id, agent = %self.primary_agent_id, "AcpNode started");
        // TODO(fusion-migrate): 在此启动 ACP stdio/server accept loop（从 ccode-shell gateway 迁入）
        Ok(())
    }

    async fn handle_message(
        &mut self,
        msg: Message,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();
        if topic.ends_with("/output") {
            // TODO(fusion-migrate): 转发为 ACP session update
            let _ = transport;
            return Ok(());
        }
        if topic.ends_with("/permission") {
            // 入站若是 ToolNode 的 PermissionRequest → 转 ACP；
            // 出站由 publish_permission_response
            return Ok(());
        }
        if topic == Topic::sys_shutdown().as_str() {
            return self.stop().await;
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            Topic::sys_shutdown().as_str().to_string(),
            Topic::agent_output(&self.primary_agent_id)
                .as_str()
                .to_string(),
            Topic::agent_permission(&self.primary_agent_id)
                .as_str()
                .to_string(),
            Topic::agent_event(&self.primary_agent_id)
                .as_str()
                .to_string(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            Topic::agent_input(&self.primary_agent_id)
                .as_str()
                .to_string(),
            Topic::agent_cancel(&self.primary_agent_id)
                .as_str()
                .to_string(),
            Topic::agent_permission(&self.primary_agent_id)
                .as_str()
                .to_string(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn graceful_stop(
        &mut self,
        _transport: Option<&NodeTransportHandle>,
    ) -> anyhow::Result<()> {
        self.stop().await
    }
}
