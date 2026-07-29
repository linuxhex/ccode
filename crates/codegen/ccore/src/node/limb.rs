//! Limb Node — 粗操作器官（肢体）
//!
//! 仿生架构中的"肢体"，负责执行命令和构建操作：
//! - 执行命令（limb/execute）
//! - 构建项目（limb/build）
//! - Git 操作（limb/git）
//!
//! Limb 执行粗粒度操作后通过 skin/touch 发布触觉反馈，
//! 同时通过 agent/{agent_id}/tool_result 返回工具结果。
//! muscle_memory 记录常用命令的输出摘要，供后续快速参考。
//! 当前只记录操作请求，不实际执行。

use async_trait::async_trait;
use std::collections::HashMap;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// Limb Node 实现
pub struct LimbNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 肌肉记忆：命令 → 输出摘要
    muscle_memory: HashMap<String, String>,
}

impl LimbNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            muscle_memory: HashMap::new(),
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
impl Node for LimbNode {
    fn node_type(&self) -> NodeType {
        NodeType::Limb
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Limb Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            "limb/execute" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let command = payload["command"].as_str().unwrap_or("");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");
                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();

                // 当前只记录，不实际执行
                let output_summary = format!("已记录命令执行请求：{}", command);
                tracing::info!("Limb {} 记录命令执行：{}", self.id, command);

                // 记录到肌肉记忆
                self.muscle_memory.insert(command.to_string(), output_summary.clone());

                // 发布触觉反馈
                self.publish_touch_feedback("limb/execute", &output_summary, true, transport).await?;

                // 发布工具结果
                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": output_summary,
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }
            }

            "limb/build" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let target = payload["target"].as_str().unwrap_or("");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");
                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();

                let output_summary = format!("已记录构建请求：{}", target);
                tracing::info!("Limb {} 记录构建请求：{}", self.id, target);

                let command_key = format!("build:{}", target);
                self.muscle_memory.insert(command_key, output_summary.clone());

                self.publish_touch_feedback("limb/build", &output_summary, true, transport).await?;

                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": output_summary,
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }
            }

            "limb/git" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let action = payload["action"].as_str().unwrap_or("");
                let agent_id = payload["agent_id"].as_str().unwrap_or("");
                let request_id = payload["request_id"].as_str().unwrap_or("").to_string();

                let output_summary = format!("已记录 Git 操作请求：{}", action);
                tracing::info!("Limb {} 记录 Git 操作：{}", self.id, action);

                let command_key = format!("git:{}", action);
                self.muscle_memory.insert(command_key, output_summary.clone());

                self.publish_touch_feedback("limb/git", &output_summary, true, transport).await?;

                if !agent_id.is_empty() {
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request_id,
                            "output": output_summary,
                            "success": true,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                }
            }

            "sys/shutdown" => {
                tracing::info!("Limb Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Limb Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "limb/execute".into(),
            "limb/build".into(),
            "limb/git".into(),
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
        tracing::info!("Limb Node 关闭：{}", self.id);
        Ok(())
    }
}
