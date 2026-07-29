//! Nose Node — 嗅觉器官
//!
//! 仿生架构中的"鼻子"，负责感知代码异味和编译问题：
//! - 嗅探代码质量问题（error/warning）
//! - 从工具执行结果（skin/touch）中提取编译输出
//! - 按严重程度分级（L0/L1/L2）
//!
//! L0 = 确定性高（编译错误），L1 = 需判断（warning），L2 = 需思考（潜在问题）

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::node::transport::NodeTransportHandle;

/// 嗅探严重程度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SmellSeverity {
    /// L0：确定性高（编译错误、链接错误）
    L0,
    /// L1：需判断（warning、lint 告警）
    L1,
    /// L2：需思考（潜在问题、代码异味）
    L2,
}

/// 嗅探结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmellResult {
    /// 严重程度
    pub severity: SmellSeverity,
    /// 嗅探消息
    pub message: String,
    /// 来源
    pub source: String,
}

/// Nose Node 实现
pub struct NoseNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 错误缓冲区
    error_buffer: Vec<SmellResult>,
}

/// error_buffer 最大容量
const ERROR_BUFFER_CAPACITY: usize = 100;

impl NoseNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            error_buffer: Vec::new(),
        }
    }

    /// 向 error_buffer 中追加嗅探结果，超出容量时移除最旧的
    fn push_smell_result(&mut self, result: SmellResult) {
        if self.error_buffer.len() >= ERROR_BUFFER_CAPACITY {
            self.error_buffer.remove(0);
        }
        self.error_buffer.push(result);
    }

    /// 解析编译输出，提取 error/warning 行
    async fn parse_compile_output(&mut self, output: &str, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        for line in output.lines() {
            let lower = line.to_lowercase();
            if lower.contains("error:") || lower.contains("error[E") {
                let result = SmellResult {
                    severity: SmellSeverity::L0,
                    message: line.to_string(),
                    source: "compile".to_string(),
                };
                self.push_smell_result(result.clone());

                // 发布编译错误
                let msg = FrameCodec::new_message(
                    Topic::nose_compile_error(),
                    self.id.as_str(),
                    &serde_json::json!({
                        "severity": "L0",
                        "message": result.message,
                        "source": result.source,
                    }),
                )?;
                // publish_data 优先，失败则走控制面
                if let Err(e) = transport.publish_data(&msg).await {
                    tracing::warn!("数据面 PUB 发送编译错误失败，回退到控制面：{}", e);
                    transport.send_message(&msg).await?;
                }
            } else if lower.contains("warning:") || lower.contains("warn[") {
                let result = SmellResult {
                    severity: SmellSeverity::L1,
                    message: line.to_string(),
                    source: "compile".to_string(),
                };
                self.push_smell_result(result);
            } else if lower.contains("note:") || lower.contains("help:") {
                let result = SmellResult {
                    severity: SmellSeverity::L2,
                    message: line.to_string(),
                    source: "compile".to_string(),
                };
                self.push_smell_result(result);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Node for NoseNode {
    fn node_type(&self) -> NodeType {
        NodeType::Nose
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!("Nose Node 启动：{}", self.id);
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            "nose/smell" => {
                // 外部触发嗅探
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let target = payload["target"].as_str().unwrap_or("");

                // 模拟嗅探：当前只记录嗅探请求
                let result = SmellResult {
                    severity: SmellSeverity::L2,
                    message: format!("嗅探请求：{}", target),
                    source: "external".to_string(),
                };
                self.push_smell_result(result);

                // 发布嗅探结果
                let smell_msg = FrameCodec::new_message(
                    Topic::nose_smell(),
                    self.id.as_str(),
                    &serde_json::json!({
                        "target": target,
                        "status": "sniffing",
                        "error_count": self.error_buffer.len(),
                    }),
                )?;
                transport.send_message(&smell_msg).await?;
            }

            "skin/touch" => {
                // 从工具执行结果中提取编译输出
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let output = payload["result"].as_str().unwrap_or("");
                let tool_name = payload["tool_name"].as_str().unwrap_or("");

                if !output.is_empty() {
                    self.parse_compile_output(output, transport).await?;
                    tracing::debug!("Nose {} 从 {} 的 touch 结果中提取编译输出", self.id, tool_name);
                }

                // 检查测试失败
                let lower = output.to_lowercase();
                if lower.contains("test result: failed") || lower.contains("failures:") {
                    let fail_msg = FrameCodec::new_message(
                        Topic::nose_test_failure(),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_name": tool_name,
                            "output": output,
                        }),
                    )?;
                    transport.send_message(&fail_msg).await?;

                    let result = SmellResult {
                        severity: SmellSeverity::L0,
                        message: format!("测试失败：{}", tool_name),
                        source: "test".to_string(),
                    };
                    self.push_smell_result(result);
                }
            }

            "sys/shutdown" => {
                tracing::info!("Nose Node 收到 shutdown 信号：{}", self.id);
            }

            _ => {
                tracing::debug!("Nose Node 收到未知 topic：{}", topic);
            }
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "nose/smell".into(),
            "skin/touch".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            "nose/compile_error".into(),
            "nose/test_failure".into(),
            "nose/smell".into(),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Nose Node 关闭：{}", self.id);
        Ok(())
    }
}
