//! Skin Node — 触觉器官
//!
//! 仿生架构中的"皮肤"，负责感知工具执行反馈：
//! - 接收 hand/* 和 limb/* 的执行结果（通配订阅）
//! - 记录工具执行历史到短期记忆（最近 50 条）
//! - 提供触觉反馈给其他 Node（如 NoseNode 从 skin/touch 提取编译输出）
//!
//! Skin 是运动-感知闭环的关键节点，类似人类皮肤感知触碰反馈。

use async_trait::async_trait;
use std::time::Instant;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// 触觉记录
#[derive(Debug, Clone)]
pub struct TouchRecord {
    /// 工具名称
    pub tool_name: String,
    /// 执行结果
    pub result: String,
    /// 是否成功
    pub success: bool,
    /// 执行时间
    #[allow(dead_code)]
    pub timestamp: Instant,
}

/// Skin Node 实现
pub struct SkinNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 短期记忆（最近 50 条触觉记录）
    short_term_memory: Vec<TouchRecord>,
}

/// short_term_memory 最大容量
const MEMORY_CAPACITY: usize = 50;

impl SkinNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            short_term_memory: Vec::with_capacity(MEMORY_CAPACITY),
        }
    }

    /// 向短期记忆中追加触觉记录，超出容量时移除最旧的
    fn push_touch_record(&mut self, record: TouchRecord) {
        if self.short_term_memory.len() >= MEMORY_CAPACITY {
            self.short_term_memory.remove(0);
        }
        self.short_term_memory.push(record);
    }
}

#[async_trait]
impl Node for SkinNode {
    fn node_type(&self) -> NodeType {
        NodeType::Skin
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Skin Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        // hand/* 和 limb/* 通配订阅：任何以 "hand/" 或 "limb/" 开头的 topic
        if topic.starts_with("hand/") || topic.starts_with("limb/") {
            let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
            let tool_name = payload["tool_name"].as_str().unwrap_or(topic).to_string();
            let result = payload["output"].as_str().or_else(|| payload["result"].as_str()).unwrap_or("").to_string();
            let success = payload["success"].as_bool().unwrap_or(true);

            let record = TouchRecord {
                tool_name: tool_name.clone(),
                result: result.clone(),
                success,
                timestamp: Instant::now(),
            };
            self.push_touch_record(record);

            // 发布触觉反馈
            let touch_msg = FrameCodec::new_message(
                Topic::skin_touch(),
                self.id.as_str(),
                &serde_json::json!({
                    "tool_name": tool_name,
                    "result": result,
                    "success": success,
                    "source_topic": topic,
                }),
            )?;
            transport.send_message(&touch_msg).await?;

            // 检查进程退出
            if !success {
                let exit_msg = FrameCodec::new_message(
                    Topic::skin_process_exit(),
                    self.id.as_str(),
                    &serde_json::json!({
                        "tool_name": tool_name,
                        "success": false,
                    }),
                )?;
                transport.send_message(&exit_msg).await?;
            }

            return Ok(());
        }

        match topic {
            "skin/touch" => {
                // 外部触觉反馈
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_name = payload["tool_name"].as_str().unwrap_or("").to_string();
                let result = payload["result"].as_str().unwrap_or("").to_string();
                let success = payload["success"].as_bool().unwrap_or(true);

                let record = TouchRecord {
                    tool_name,
                    result,
                    success,
                    timestamp: Instant::now(),
                };
                self.push_touch_record(record);
                tracing::debug!("Skin {} 收到外部触觉反馈", self.id);
            }

            "sys/shutdown" => {
                tracing::info!("Skin Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Skin Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "hand/*".into(),
            "limb/*".into(),
            "skin/touch".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            "skin/touch".into(),
            "skin/process_exit".into(),
            "skin/memory_pressure".into(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Skin Node 关闭：{}", self.id);
        Ok(())
    }
}
