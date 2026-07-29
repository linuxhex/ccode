//! Eye Node — 视觉器官
//!
//! 仿生架构中的"眼睛"，负责观察外部世界：
//! - 读取文件/目录内容（观察）
//! - 监听文件变化事件
//! - 捕获终端输出流
//!
//! 观察结果存入 buffer（最近 10 条），供 ThinkerNode 感官综合使用。

use async_trait::async_trait;
use std::time::Instant;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// 观察记录
#[derive(Debug, Clone)]
pub struct Observation {
    /// 观察来源 topic
    pub topic: String,
    /// 观察内容
    pub content: String,
    /// 观察时间
    #[allow(dead_code)]
    pub timestamp: Instant,
}

/// Eye Node 实现
pub struct EyeNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 观察缓冲区（最近 10 条）
    buffer: Vec<Observation>,
}

/// buffer 最大容量
const BUFFER_CAPACITY: usize = 10;

impl EyeNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            buffer: Vec::with_capacity(BUFFER_CAPACITY),
        }
    }

    /// 向 buffer 中追加观察记录，超出容量时移除最旧的
    fn push_observation(&mut self, obs: Observation) {
        if self.buffer.len() >= BUFFER_CAPACITY {
            self.buffer.remove(0);
        }
        self.buffer.push(obs);
    }
}

#[async_trait]
impl Node for EyeNode {
    fn node_type(&self) -> NodeType {
        NodeType::Eye
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Eye Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            "eye/observe" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let path = payload["path"].as_str().unwrap_or("");
                let obs_type = payload["type"].as_str().unwrap_or("file");

                // 模拟读取文件/目录
                let content = format!("[Eye] 观察 {}（类型={}）：已读取内容", path, obs_type);
                let obs = Observation {
                    topic: topic.to_string(),
                    content: content.clone(),
                    timestamp: Instant::now(),
                };
                self.push_observation(obs);

                // 发布观察结果
                let result_msg = FrameCodec::new_message(
                    Topic::eye_observe(),
                    self.id.as_str(),
                    &serde_json::json!({
                        "path": path,
                        "type": obs_type,
                        "content": content,
                        "observer": self.id.as_str(),
                    }),
                )?;
                transport.send_message(&result_msg).await?;
            }

            "eye/file_changed" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let path = payload["path"].as_str().unwrap_or("");
                let change_type = payload["change_type"].as_str().unwrap_or("modified");

                let content = format!("[Eye] 文件变化：{} ({})", path, change_type);
                let obs = Observation {
                    topic: topic.to_string(),
                    content: content.clone(),
                    timestamp: Instant::now(),
                };
                self.push_observation(obs);

                // 发布 reindex 通知
                let reindex_msg = FrameCodec::new_message(
                    Topic::eye_observe(),
                    self.id.as_str(),
                    &serde_json::json!({
                        "action": "reindex",
                        "path": path,
                        "change_type": change_type,
                    }),
                )?;
                transport.send_message(&reindex_msg).await?;
            }

            "eye/terminal_output" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let output = payload["content"].as_str().unwrap_or("");

                let obs = Observation {
                    topic: topic.to_string(),
                    content: output.to_string(),
                    timestamp: Instant::now(),
                };
                self.push_observation(obs);
                tracing::debug!("Eye {} 记录终端输出，长度={}", self.id, output.len());
            }

            "sys/shutdown" => {
                tracing::info!("Eye Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Eye Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "eye/observe".into(),
            "eye/file_changed".into(),
            "eye/terminal_output".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec!["eye/observe".into()]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Eye Node 关闭：{}", self.id);
        Ok(())
    }
}
