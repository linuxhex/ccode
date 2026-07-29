//! Hand Node — 精细操作器官（手）
//!
//! 仿生架构中的"手"，负责代码精细操作：
//! - 编辑文件（hand/edit）
//! - 搜索代码（hand/search）
//! - 重构代码（hand/restructure）
//!
//! Hand 执行精细操作后通过 skin/touch 发布触觉反馈，
//! 同时通过 agent/{agent_id}/tool_result 返回工具结果。
//! 当前只记录操作请求，不实际执行。

use async_trait::async_trait;
use std::collections::HashMap;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType, PermissionMode};
use crate::node::transport::NodeTransportHandle;

/// Hand Node 实现
pub struct HandNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 权限模式
    permission_mode: PermissionMode,
    /// 等待中的结果（request_id → 状态描述）
    pending_results: HashMap<String, String>,
}

impl HandNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            permission_mode: PermissionMode::Trust,
            pending_results: HashMap::new(),
        }
    }

    /// 设置权限模式
    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        self.permission_mode = mode;
    }

    /// 权限检查：根据模式决定是否允许操作
    fn check_permission(&self, operation: &str) -> bool {
        match self.permission_mode {
            PermissionMode::Yolo => true,
            PermissionMode::Trust => true,
            PermissionMode::Ask => {
                tracing::warn!("Hand {} 操作 {} 需要 Ask 模式确认，当前自动允许", self.id, operation);
                true
            }
        }
    }

    /// 发布触觉反馈到 skin/touch
    async fn publish_touch_feedback(
        &self,
        tool_name: &str,
        result: &str,
        success: bool,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let touch_msg = FrameCodec::new_message(
            Topic::skin_touch(),
            self.id.as_str(),
            &serde_json::json!({
                "tool_name": tool_name,
                "result": result,
                "success": success,
            }),
        )?;
        transport.send_message(&touch_msg).await?;
        Ok(())
    }
}

#[async_trait]
impl Node for HandNode {
    fn node_type(&self) -> NodeType {
        NodeType::Hand
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Hand Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            "hand/edit" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let path = payload["path"].as_str().unwrap_or("");
                let operation = payload["operation"].as_str().unwrap_or("replace");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");

                if !self.check_permission(&format!("edit:{}", path)) {
                    return Ok(());
                }

                // 当前只记录，不实际执行
                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();
                let desc = format!("编辑 {} ({})", path, operation);
                self.pending_results.insert(request_id.clone(), desc.clone());
                tracing::info!("Hand {} 记录编辑请求：{}", self.id, desc);

                // 发布触觉反馈
                self.publish_touch_feedback("hand/edit", &format!("已记录编辑请求：{}", desc), true, transport).await?;

                // 发布工具结果
                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": format!("已记录编辑请求：{}", desc),
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }

                self.pending_results.remove(&request_id);
            }

            "hand/search" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let query = payload["query"].as_str().unwrap_or("");
                let search_type = payload["type"].as_str().unwrap_or("semantic");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");

                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();
                let desc = format!("搜索 {} ({})", query, search_type);
                self.pending_results.insert(request_id.clone(), desc.clone());
                tracing::info!("Hand {} 记录搜索请求：{}", self.id, desc);

                self.publish_touch_feedback("hand/search", &format!("已记录搜索请求：{}", desc), true, transport).await?;

                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": format!("已记录搜索请求：{}", desc),
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }

                self.pending_results.remove(&request_id);
            }

            "hand/restructure" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let target = payload["target"].as_str().unwrap_or("");
                let action = payload["action"].as_str().unwrap_or("move");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");

                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();
                let desc = format!("重构 {} ({})", target, action);
                self.pending_results.insert(request_id.clone(), desc.clone());
                tracing::info!("Hand {} 记录重构请求：{}", self.id, desc);

                self.publish_touch_feedback("hand/restructure", &format!("已记录重构请求：{}", desc), true, transport).await?;

                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": format!("已记录重构请求：{}", desc),
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }

                self.pending_results.remove(&request_id);
            }

            "sys/shutdown" => {
                tracing::info!("Hand Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Hand Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "hand/edit".into(),
            "hand/search".into(),
            "hand/restructure".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            "skin/touch".into(),
            "agent/*/tool_result".into(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Hand Node 关闭：{}", self.id);
        Ok(())
    }
}
