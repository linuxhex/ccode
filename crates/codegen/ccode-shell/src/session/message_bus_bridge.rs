//! MessageBusBridge - ccode-shell 与 ccore 消息总线的桥接器
//!
//! SessionActor 始终通过此桥接器将 LLM 请求和工具调用路由到消息总线上的 SamplerNode
//! 和 ToolNode。如果桥接器不可用，则回退到直接调用 ccode-sampler/ccode-tools。
//!
//! 架构（分布式模式统一后）：
//! ```text
//! SessionActor
//!   └─ MessageBusBridge（主路径）
//!      ├─ send_llm_request() → 消息总线 → SamplerNode
//!      ├─ send_tool_call()   → 消息总线 → ToolNode
//!      ├─ broadcast_state()  → 消息总线 → TUI/监控
//!      └─ 回退：桥接器不可用时 → 本地 ccode-sampler/ccode-tools
//! ```
//!
//! 桥接器内部运行一个消息循环（类似 ccore 的 `run_node`），负责：
//! - 接收消息总线上的响应并路由到等待中的调用方
//! - 定期发送心跳
//! - 处理 publisher 发现

use std::collections::HashMap;
use std::sync::Arc;

use ccore::message::frame::FrameCodec;
use ccore::message::Topic;
use ccore::node::transport::{NodeConnectInfo, NodeTransport};
use ccore::node::NodeId;
use ccore::sampler::provider::StreamChunk;
use tokio::sync::{mpsc, oneshot};

/// LLM 请求条目（通过通道发送给桥接循环）
struct LlmRequestEntry {
    /// 完整的采样请求
    request: ccore::sampler::provider::SampleRequest,
    /// 流式 chunk 接收端（转发给调用方）
    chunk_tx: mpsc::Sender<StreamChunk>,
    /// 完成信号（done/error）
    done_tx: oneshot::Sender<LlmOutcome>,
}

/// LLM 请求最终结果
pub enum LlmOutcome {
    /// 正常完成
    Done,
    /// 采样错误
    Error(String),
}

/// 工具调用条目（通过通道发送给桥接循环）
struct ToolCallEntry {
    /// 工具调用 ID
    tool_call_id: String,
    /// 工具名称
    tool_name: String,
    /// 工具参数
    arguments: serde_json::Value,
    /// 结果接收端
    result_tx: oneshot::Sender<Result<String, String>>,
}

/// 状态变迁广播条目
struct StateBroadcastEntry {
    /// 状态名称（Idle/Thinking/ToolCalling/...）
    state: String,
    /// 附加元数据
    metadata: serde_json::Value,
}

/// MessageBusBridge - SessionActor 与消息总线的桥接器
///
/// 通过内部 mpsc 通道与后台消息循环通信，调用方使用 async 方法等待响应。
/// 桥接器自动处理心跳、publisher 发现和消息路由。
pub struct MessageBusBridge {
    /// LLM 请求发送通道
    llm_request_tx: mpsc::Sender<LlmRequestEntry>,
    /// 工具调用发送通道
    tool_call_tx: mpsc::Sender<ToolCallEntry>,
    /// 状态广播发送通道
    state_broadcast_tx: mpsc::Sender<StateBroadcastEntry>,
    /// Agent ID
    agent_id: String,
}

impl MessageBusBridge {
    /// 连接到消息总线并创建桥接器
    ///
    /// 创建 NodeTransport 连接到 Kernel，启动后台消息循环。
    /// 返回的 Arc<Self> 可被 SessionActor 持有。
    pub async fn connect(
        router_addr: &str,
        pub_addr: &str,
        agent_id: String,
    ) -> anyhow::Result<Arc<Self>> {
        let node_id: NodeId = agent_id
            .parse()
            .map_err(|_| anyhow::anyhow!("无效的 agent_id: {}", agent_id))?;

        let subscriptions = vec![
            format!("agent/{}/tool_result", agent_id),
            "sampler/*/stream".into(),
            "tool/register".into(),
            "sys/shutdown".into(),
            "sys/publisher_change".into(),
        ];

        let published_topics = vec![
            format!("agent/{}/output", agent_id),
            format!("agent/{}/tool_call", agent_id),
            format!("agent/{}/state_change", agent_id),
        ];

        let connect_info = NodeConnectInfo {
            router_addr: router_addr.to_string(),
            pub_addr: pub_addr.to_string(),
            node_id,
            node_type: "agent".to_string(),
            subscriptions,
            data_pub_addr: format!("ipc:///tmp/ccode-shell-pub-{}", agent_id),
            published_topics,
            data_rep_addr: None,
            service_name: None,
        };

        let transport = NodeTransport::connect(&connect_info).await?;
        tracing::info!("MessageBusBridge 已连接：agent_id={}", agent_id);

        let (llm_request_tx, llm_request_rx) = mpsc::channel::<LlmRequestEntry>(64);
        let (tool_call_tx, tool_call_rx) = mpsc::channel::<ToolCallEntry>(64);
        let (state_broadcast_tx, state_broadcast_rx) =
            mpsc::channel::<StateBroadcastEntry>(64);

        let bridge = Arc::new(Self {
            llm_request_tx,
            tool_call_tx,
            state_broadcast_tx,
            agent_id: agent_id.clone(),
        });

        // 启动后台消息循环
        tokio::spawn(Self::run_bridge_loop(
            transport,
            agent_id,
            llm_request_rx,
            tool_call_rx,
            state_broadcast_rx,
        ));

        Ok(bridge)
    }

    /// 发送 LLM 请求到消息总线（通过 SamplerNode 处理）
    ///
    /// 返回流式 chunk 接收器和完成信号。
    /// 调用方应先消费 chunk 流，再 await 完成信号获取最终状态。
    pub async fn send_llm_request(
        &self,
        request: ccore::sampler::provider::SampleRequest,
    ) -> anyhow::Result<(
        mpsc::Receiver<StreamChunk>,
        oneshot::Receiver<LlmOutcome>,
    )> {
        let (chunk_tx, chunk_rx) = mpsc::channel::<StreamChunk>(64);
        let (done_tx, done_rx) = oneshot::channel::<LlmOutcome>();

        self.llm_request_tx
            .send(LlmRequestEntry {
                request,
                chunk_tx,
                done_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("MessageBusBridge 已关闭"))?;

        Ok((chunk_rx, done_rx))
    }

    /// 发送工具调用到消息总线（通过 ToolNode 处理）
    ///
    /// 返回工具执行结果。
    pub async fn send_tool_call(
        &self,
        tool_call_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    ) -> anyhow::Result<Result<String, String>> {
        let (result_tx, result_rx) = oneshot::channel::<Result<String, String>>();

        self.tool_call_tx
            .send(ToolCallEntry {
                tool_call_id,
                tool_name,
                arguments,
                result_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("MessageBusBridge 已关闭"))?;

        result_rx
            .await
            .map_err(|_| anyhow::anyhow!("MessageBusBridge 已关闭"))
    }

    /// 广播 Agent 状态变迁到消息总线
    ///
    /// TUI Node 和监控工具可订阅 `agent/{id}/state_change` 获取状态更新。
    pub async fn broadcast_state(
        &self,
        state: impl Into<String>,
        metadata: serde_json::Value,
    ) -> anyhow::Result<()> {
        self.state_broadcast_tx
            .send(StateBroadcastEntry {
                state: state.into(),
                metadata,
            })
            .await
            .map_err(|_| anyhow::anyhow!("MessageBusBridge 已关闭"))?;
        Ok(())
    }

    /// 获取 Agent ID
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// 桥接器后台消息循环
    ///
    /// 负责：
    /// - 接收消息总线上的响应并路由到等待中的调用方
    /// - 处理新的 LLM 请求/工具调用（编码并发送）
    /// - 定期发送心跳
    /// - 处理 publisher 发现
    async fn run_bridge_loop(
        mut transport: NodeTransport,
        agent_id: String,
        mut llm_request_rx: mpsc::Receiver<LlmRequestEntry>,
        mut tool_call_rx: mpsc::Receiver<ToolCallEntry>,
        mut state_broadcast_rx: mpsc::Receiver<StateBroadcastEntry>,
    ) {
        // 等待中的 LLM 请求：request_id → entry
        let mut pending_llm: HashMap<String, LlmRequestEntry> = HashMap::new();
        // 等待中的工具调用：tool_call_id → result_tx
        let mut pending_tools: HashMap<String, oneshot::Sender<Result<String, String>>> =
            HashMap::new();

        let handle = transport.handle().clone();
        let mut heartbeat_timer = tokio::time::interval(std::time::Duration::from_secs(10));
        // 跳过首次立即触发
        heartbeat_timer.tick().await;

        tracing::info!("MessageBusBridge 消息循环已启动：agent_id={}", agent_id);

        loop {
            tokio::select! {
                // 接收消息总线消息
                msg_result = transport.recv() => {
                    match msg_result {
                        Ok(Some(msg)) => {
                            Self::handle_incoming_message(
                                &msg,
                                &agent_id,
                                &mut pending_llm,
                                &mut pending_tools,
                            ).await;
                        }
                        Ok(None) => {
                            tracing::warn!("MessageBusBridge 传输层通道已关闭");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("MessageBusBridge 传输层接收错误：{}", e);
                            break;
                        }
                    }
                }
                // 新的 LLM 请求
                entry = llm_request_rx.recv() => {
                    if let Some(entry) = entry {
                        let request_id = entry.request.request_id.clone();
                        // 编码并发送采样请求
                        match FrameCodec::new_message(
                            Topic::sampler_request(),
                            &agent_id,
                            &entry.request,
                        ) {
                            Ok(msg) => {
                                // 优先数据面 PUB，回退控制面
                                if let Err(e) = handle.publish_data(&msg).await {
                                    tracing::debug!("LLM 请求 PUB 失败，回退控制面：{}", e);
                                    if let Err(e) = handle.send_message(&msg).await {
                                        tracing::warn!("LLM 请求发送失败：{}", e);
                                        let _ = entry.done_tx.send(LlmOutcome::Error(
                                            format!("发送失败：{}", e)
                                        ));
                                        continue;
                                    }
                                }
                                pending_llm.insert(request_id.clone(), entry);
                                tracing::debug!("LLM 请求已发送：{}", request_id);
                            }
                            Err(e) => {
                                tracing::warn!("LLM 请求编码失败：{}", e);
                                let _ = entry.done_tx.send(LlmOutcome::Error(
                                    format!("编码失败：{}", e)
                                ));
                            }
                        }
                    }
                }
                // 新的工具调用
                entry = tool_call_rx.recv() => {
                    if let Some(entry) = entry {
                        let tool_call_id = entry.tool_call_id.clone();
                        let payload = serde_json::json!({
                            "tool_call_id": entry.tool_call_id,
                            "tool_name": entry.tool_name,
                            "arguments": entry.arguments,
                            "agent_id": agent_id,
                        });
                        match FrameCodec::new_message(
                            Topic::agent_tool_call(&agent_id),
                            &agent_id,
                            &payload,
                        ) {
                            Ok(msg) => {
                                if let Err(e) = handle.publish_data(&msg).await {
                                    tracing::debug!("工具调用 PUB 失败，回退控制面：{}", e);
                                    if let Err(e) = handle.send_message(&msg).await {
                                        tracing::warn!("工具调用发送失败：{}", e);
                                        let _ = entry.result_tx.send(Err(format!("发送失败：{}", e)));
                                        continue;
                                    }
                                }
                                pending_tools.insert(tool_call_id, entry.result_tx);
                                tracing::debug!("工具调用已发送：{}", payload["tool_name"]);
                            }
                            Err(e) => {
                                tracing::warn!("工具调用编码失败：{}", e);
                                let _ = entry.result_tx.send(Err(format!("编码失败：{}", e)));
                            }
                        }
                    }
                }
                // 状态变迁广播
                entry = state_broadcast_rx.recv() => {
                    if let Some(entry) = entry {
                        let payload = serde_json::json!({
                            "agent_id": agent_id,
                            "state": entry.state,
                            "metadata": entry.metadata,
                            "timestamp": chrono::Utc::now().to_rfc3339(),
                        });
                        if let Ok(msg) = FrameCodec::new_message(
                            Topic::agent_event(&agent_id),
                            &agent_id,
                            &payload,
                        ) {
                            // 状态变迁走控制面（小消息，确保可靠传输）
                            if let Err(e) = handle.send_message(&msg).await {
                                tracing::warn!("状态变迁广播失败：{}", e);
                            }
                        }
                    }
                }
                // 心跳
                _ = heartbeat_timer.tick() => {
                    let heartbeat = serde_json::json!({
                        "node_id": agent_id,
                    });
                    if let Ok(msg) = FrameCodec::new_message(
                        Topic::sys_heartbeat(),
                        &agent_id,
                        &heartbeat,
                    ) {
                        if let Err(e) = handle.send_message(&msg).await {
                            tracing::warn!("心跳发送失败：{}", e);
                        }
                    }
                }
            }
        }

        // 通知所有等待中的调用方桥接器已关闭
        for (_, entry) in pending_llm.drain() {
            let _ = entry.done_tx.send(LlmOutcome::Error("桥接器已关闭".into()));
        }
        for (_, tx) in pending_tools.drain() {
            let _ = tx.send(Err("桥接器已关闭".into()));
        }

        transport.shutdown().await;
        tracing::info!("MessageBusBridge 消息循环已退出：agent_id={}", agent_id);
    }

    /// 处理收到的消息，路由到等待中的调用方
    async fn handle_incoming_message(
        msg: &ccore::message::Message,
        agent_id: &str,
        pending_llm: &mut HashMap<String, LlmRequestEntry>,
        pending_tools: &mut HashMap<String, oneshot::Sender<Result<String, String>>>,
    ) {
        let topic = msg.topic.as_str();

        // LLM 流式响应
        if topic.starts_with("sampler/") && topic.ends_with("/stream") {
            // 先解码为 JSON 检查消息类型（done/error/stream chunk）
            let raw_value: serde_json::Value = match FrameCodec::decode_payload(msg) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("解码 sampler 流消息失败：{}", e);
                    return;
                }
            };

            // done 消息
            if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                    if let Some(entry) = pending_llm.remove(req_id) {
                        let _ = entry.done_tx.send(LlmOutcome::Done);
                        tracing::debug!("LLM 采样完成：{}", req_id);
                    }
                }
                return;
            }

            // error 消息
            if let Some(err) = raw_value.get("error") {
                let err_msg = err.as_str().unwrap_or("unknown error");
                if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                    if let Some(entry) = pending_llm.remove(req_id) {
                        let _ = entry.done_tx.send(LlmOutcome::Error(err_msg.into()));
                        tracing::warn!("LLM 采样错误：{} - {}", req_id, err_msg);
                    }
                }
                return;
            }

            // 正常 StreamChunk
            match serde_json::from_value::<StreamChunk>(raw_value.clone()) {
                Ok(chunk) => {
                    let req_id = chunk.request_id.clone();
                    if let Some(entry) = pending_llm.get(&req_id) {
                        // 转发给等待中的调用方
                        if entry.chunk_tx.send(chunk).await.is_err() {
                            tracing::warn!("调用方已丢弃 chunk 流：{}", req_id);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("解码 StreamChunk 失败：{}", e);
                }
            }
            return;
        }

        // 工具执行结果
        if topic.ends_with("/tool_result") {
            let payload: serde_json::Value = match FrameCodec::decode_payload(msg) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("解码 tool_result 失败：{}", e);
                    return;
                }
            };
            let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
            let output = payload["output"].as_str().unwrap_or("");
            let success = payload["success"].as_bool().unwrap_or(true);

            if let Some(tx) = pending_tools.remove(tool_call_id) {
                if success {
                    let _ = tx.send(Ok(output.to_string()));
                } else {
                    let _ = tx.send(Err(output.to_string()));
                }
                tracing::debug!("工具结果已路由：{}", tool_call_id);
            }
            return;
        }

        // 系统关闭
        if topic == "sys/shutdown" {
            tracing::info!("MessageBusBridge 收到关闭信号");
            return;
        }

        // publisher_change 通知（忽略，由 transport 层处理）
        if topic == "sys/publisher_change" {
            return;
        }

        // 其他消息忽略
        tracing::trace!("MessageBusBridge 忽略消息：{}", topic);
    }
}
