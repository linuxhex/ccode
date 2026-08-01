//! AcpNode — stdio JSON-RPC 与消息总线之间的边界
//!
//! 职责：
//! - stdin JSON-RPC reader：解析 `session/prompt`、`session/cancel`、`initialize`
//! - stdout JSON-RPC writer：将 agent output 格式化为 JSON-RPC notifications
//! - 总线桥接：stdin → `agent/{id}/input`、`agent/{id}/cancel`
//! - 总线桥接：`agent/{id}/output` → stdout
//!
//! 纯总线模式：AcpNode 只通过 topic 与其他 Node 通信，无旁路通道。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::node::transport::NodeTransportHandle;
use crate::node::{Node, NodeContext, NodeId, NodeType};

// ── JSON-RPC 2.0 轻量类型（不依赖外部 ACP crate） ──

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<serde_json::Value>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: serde_json::Value,
    result: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    params: serde_json::Value,
}

// ── AcpNode ──

pub struct AcpNode {
    id: NodeId,
    primary_agent_id: String,
    stdout_tx: Option<mpsc::UnboundedSender<serde_json::Value>>,
}

impl AcpNode {
    pub fn new(id: NodeId, primary_agent_id: impl Into<String>) -> Self {
        Self {
            id,
            primary_agent_id: primary_agent_id.into(),
            stdout_tx: None,
        }
    }

    pub fn set_primary_agent(&mut self, agent_id: impl Into<String>) {
        self.primary_agent_id = agent_id.into();
    }

    fn spawn_stdio_tasks(
        &mut self,
        transport: &NodeTransportHandle,
    ) {
        if self.stdout_tx.is_some() {
            return;
        }

        let (stdout_tx, mut stdout_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        self.stdout_tx = Some(stdout_tx.clone());

        let primary_agent_id = self.primary_agent_id.clone();
        let node_id_str = self.id.as_str().to_string();
        let transport = transport.clone();
        let stdin_stdout_tx = stdout_tx;

        // stdin reader: 读 JSON-RPC → 发布到总线 / 写响应到 stdout
        tokio::spawn(async move {
            let stdin = tokio::io::stdin();
            let mut reader = BufReader::new(stdin);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => {
                        tracing::info!("AcpNode: stdin EOF");
                        break;
                    }
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(req) = serde_json::from_str::<JsonRpcRequest>(trimmed) {
                            Self::handle_jsonrpc_request(
                                &req,
                                &primary_agent_id,
                                &node_id_str,
                                &transport,
                                &stdin_stdout_tx,
                            )
                            .await;
                        } else {
                            tracing::warn!("AcpNode: 无法解析 JSON-RPC: {}", trimmed);
                        }
                    }
                    Err(e) => {
                        tracing::error!("AcpNode: stdin 读取失败: {}", e);
                        break;
                    }
                }
            }
        });

        // stdout writer: 从 channel 接收 → 写 JSON-RPC 到 stdout
        tokio::spawn(async move {
            let stdout = tokio::io::stdout();
            let mut writer = stdout;

            while let Some(value) = stdout_rx.recv().await {
                let mut json_line = match serde_json::to_string(&value) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("AcpNode: JSON 序列化失败: {}", e);
                        continue;
                    }
                };
                json_line.push('\n');
                if let Err(e) = writer.write_all(json_line.as_bytes()).await {
                    tracing::error!("AcpNode: stdout 写入失败: {}", e);
                    break;
                }
                let _ = writer.flush().await;
            }
        });
    }

    async fn handle_jsonrpc_request(
        req: &JsonRpcRequest,
        primary_agent_id: &str,
        node_id_str: &str,
        transport: &NodeTransportHandle,
        stdout_tx: &mpsc::UnboundedSender<serde_json::Value>,
    ) {
        match req.method.as_str() {
            "initialize" => {
                if let Some(id) = &req.id {
                    let response = JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: id.clone(),
                        result: serde_json::json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": {
                                "session": {}
                            },
                            "agentInfo": {
                                "name": "ccore-fusion",
                                "version": "0.1.0"
                            }
                        }),
                    };
                    let _ = stdout_tx.send(serde_json::to_value(response).unwrap_or_default());
                }
            }
            "session/prompt" => {
                let text = req.params["prompt"]
                    .as_str()
                    .or_else(|| req.params["text"].as_str())
                    .unwrap_or("");
                let session_id = req.params["sessionId"].as_str().unwrap_or("default");

                let payload = serde_json::json!({
                    "content": text,
                    "role": "user",
                    "session_id": session_id,
                });
                if let Ok(msg) = FrameCodec::new_message(
                    Topic::agent_input(primary_agent_id),
                    node_id_str,
                    &payload,
                ) {
                    let _ = transport.publish_data(&msg).await;
                }
            }
            "session/cancel" => {
                let session_id = req.params["sessionId"].as_str().unwrap_or("default");
                let payload = serde_json::json!({
                    "agent_id": primary_agent_id,
                    "reason": "user_cancelled",
                    "session_id": session_id,
                });
                if let Ok(msg) = FrameCodec::new_message(
                    Topic::agent_cancel(primary_agent_id),
                    node_id_str,
                    &payload,
                ) {
                    let _ = transport.publish_data(&msg).await;
                }
            }
            _ => {
                tracing::debug!("AcpNode: 未处理的 JSON-RPC method: {}", req.method);
                if let Some(id) = &req.id {
                    let response = JsonRpcResponse {
                        jsonrpc: "2.0",
                        id: id.clone(),
                        result: serde_json::json!({"error": "method not found"}),
                    };
                    let _ = stdout_tx.send(serde_json::to_value(response).unwrap_or_default());
                }
            }
        }
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
        tracing::info!(
            node_id = %self.id,
            agent = %self.primary_agent_id,
            "AcpNode started (stdio JSON-RPC mode)"
        );
        Ok(())
    }

    async fn handle_message(
        &mut self,
        msg: Message,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        // 首次调用时启动 stdio tasks
        self.spawn_stdio_tasks(transport);

        let topic = msg.topic.as_str();

        // agent/{id}/output → stdout JSON-RPC notification
        if topic.ends_with("/output") {
            let payload: serde_json::Value = serde_json::from_slice(&msg.payload)
                .unwrap_or_default();
            let channel = payload["channel"].as_str().unwrap_or("text");
            let content = payload["content"].as_str().unwrap_or("");

            let notification = JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/update".into(),
                params: serde_json::json!({
                    "sessionId": "default",
                    "update": {
                        "type": if channel == "text" { "text" } else { "tool_call" },
                        "content": content,
                    }
                }),
            };
            if let Some(tx) = &self.stdout_tx {
                let _ = tx.send(serde_json::to_value(notification).unwrap_or_default());
            }
            return Ok(());
        }

        // agent/{id}/permission → stdout JSON-RPC notification
        if topic.ends_with("/permission") {
            let payload: serde_json::Value = serde_json::from_slice(&msg.payload)
                .unwrap_or_default();
            let notification = JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/update".into(),
                params: serde_json::json!({
                    "sessionId": "default",
                    "update": {
                        "type": "permission_request",
                        "content": payload,
                    }
                }),
            };
            if let Some(tx) = &self.stdout_tx {
                let _ = tx.send(serde_json::to_value(notification).unwrap_or_default());
            }
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
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        // 发送 session/end 通知
        if let Some(tx) = &self.stdout_tx {
            let notification = JsonRpcNotification {
                jsonrpc: "2.0",
                method: "session/end".into(),
                params: serde_json::json!({
                    "sessionId": "default",
                }),
            };
            let _ = tx.send(serde_json::to_value(notification).unwrap_or_default());
        }
        tracing::info!("AcpNode 关闭：{}", self.id);
        Ok(())
    }

    async fn graceful_stop(
        &mut self,
        _transport: Option<&NodeTransportHandle>,
    ) -> anyhow::Result<()> {
        self.stop().await
    }
}
