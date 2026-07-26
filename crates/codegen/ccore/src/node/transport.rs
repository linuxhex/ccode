//! Node 端 ZMQ 传输层（ROS 1 风格双面架构）
//!
//! ROS 1 风格双面通信：
//! - 控制面：DEALER → Kernel ROUTER（注册、发现、心跳、参数）
//! - 数据面：PUB → 其他 Node SUB（业务数据点对点传输）
//! - 控制面广播：SUB → Kernel PUB（系统消息广播）
//!
//! 注册流程（ROS 1 风格）：
//! 1. Node 创建 DEALER + SUB socket 连接 Kernel
//! 2. 发送 sys/register 消息（含 pub_addr、published_topics）
//! 3. Kernel 返回 publisher_discovery 响应
//! 4. Node 对发现的 publisher 发起 SUB 连接（数据面直连）
//!
//! zeromq 0.4 适配：
//! - DealerSocket 不再支持 split()，使用 tokio::select! 交替收发
//! - ZmqMessage 从 Vec<Bytes> 构造使用 try_from()

use bytes::Bytes;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeromq::{DealerSocket, Socket, SocketRecv, SocketSend, SubSocket, ZmqMessage};

use crate::message::frame::FrameCodec;
use crate::message::{Message, SequenceManager};
use crate::node::NodeId;

/// 发现的 Publisher 信息（Kernel 通过 publisher_discovery 响应下发）
#[derive(Debug, Clone)]
pub struct PublisherInfo {
    /// Publisher 的 Node ID
    pub node_id: String,
    /// Publisher 的数据面 PUB 地址
    pub pub_addr: String,
    /// Publisher 发布的 topic 列表
    pub topics: Vec<String>,
}

/// 数据面状态 — 缓存已发现的 Publisher 信息
///
/// 当前阶段仅缓存，为后续 Node 间 PUB/SUB 直连做准备。
/// 未来实现直连时，此处将维护 SUB socket 连接池。
#[derive(Debug, Default)]
pub struct DataPlaneState {
    /// 已发现的 publisher：pub_addr → PublisherInfo
    publishers: HashMap<String, PublisherInfo>,
}

impl DataPlaneState {
    /// 创建空的数据面状态
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理 Kernel 返回的 publisher_discovery 响应
    ///
    /// 解析响应中的 publisher 列表，缓存到本地状态。
    /// 当前的 PUB/SUB 直连尚未实现，此处仅记录日志和缓存信息。
    pub fn handle_discovery(&mut self, payload: &serde_json::Value) {
        let publishers = match payload.get("publishers").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return,
        };

        for entry in publishers {
            if let Some(pub_list) = entry.get("publishers").and_then(|v| v.as_array()) {
                for p in pub_list {
                    let node_id = p["node_id"].as_str().unwrap_or("").to_string();
                    let pub_addr = p["pub_addr"].as_str().unwrap_or("").to_string();
                    let topics = p["topics"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|t| t.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();

                    if pub_addr.is_empty() {
                        continue;
                    }

                    tracing::info!(
                        "发现 Publisher：node={}, addr={}, topics={:?}",
                        node_id, pub_addr, topics
                    );
                    self.publishers.insert(
                        pub_addr.clone(),
                        PublisherInfo { node_id, pub_addr, topics },
                    );
                }
            }
        }
    }

    /// 获取所有已发现的 publisher 信息
    pub fn publishers(&self) -> &HashMap<String, PublisherInfo> {
        &self.publishers
    }
}

/// Node 端传输层句柄
///
/// Node 通过此句柄发送消息到消息总线。
/// 内部通过 mpsc 通道与后台 DEALER 任务通信。
#[derive(Clone)]
pub struct NodeTransportHandle {
    /// 发送消息的通道
    outgoing_tx: mpsc::Sender<Vec<Bytes>>,
    /// 序列号管理器
    sequence_manager: Arc<SequenceManager>,
}

impl NodeTransportHandle {
    /// 发送消息到消息总线（自动添加序列号）
    ///
    /// 将 ccode Message 编码为 3 帧格式（topic + header + payload），
    /// 通过 DEALER socket 发送到 Kernel ROUTER。
    pub async fn send_message(&self, msg: &Message) -> anyhow::Result<()> {
        // 获取下一个序列号
        let sequence = self.sequence_manager.next_sequence();

        // 创建带序列号的消息
        let msg_with_seq = Message {
            topic: msg.topic.clone(),
            header: crate::message::MessageHeader {
                msg_id: msg.header.msg_id.clone(),
                timestamp: msg.header.timestamp.clone(),
                src_node: msg.header.src_node.clone(),
                reply_to: msg.header.reply_to.clone(),
                sequence,
            },
            payload: msg.payload.clone(),
        };

        let frames = FrameCodec::encode(&msg_with_seq)?;
        let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
        self.outgoing_tx
            .send(bytes_frames)
            .await
            .map_err(|_| anyhow::anyhow!("发送通道已关闭"))
    }

    /// 发送原始帧到消息总线（跳过序列号）
    pub async fn send_frames(&self, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(frames)
            .await
            .map_err(|_| anyhow::anyhow!("发送通道已关闭"))
    }

    /// 获取当前序列号
    pub fn current_sequence(&self) -> u64 {
        self.sequence_manager.current_sequence()
    }
}

/// Node 端传输层
///
/// 管理 DEALER + SUB socket 的异步 I/O。
/// 启动后通过通道提供消息收发接口。
pub struct NodeTransport {
    /// 接收通道 — 后台 recv 任务 → Node
    incoming_rx: mpsc::Receiver<Message>,
    /// 发送句柄 — 供 Node 发送消息
    handle: NodeTransportHandle,
    /// 后台任务 JoinHandle
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// Node 传输层连接参数
///
/// 包含连接消息总线所需的全部信息，由 Node 提供。
pub struct NodeConnectInfo {
    /// Kernel 的 ROUTER socket 地址
    pub router_addr: String,
    /// Kernel 的 PUB socket 地址
    pub pub_addr: String,
    /// 本 Node 的 ID
    pub node_id: NodeId,
    /// 本 Node 的类型名称（如 "agent"、"tool" 等）
    pub node_type: String,
    /// 本 Node 订阅的 topic 模式列表
    pub subscriptions: Vec<String>,
    /// 本 Node 的数据面 PUB 地址（空字符串表示不发布）
    pub data_pub_addr: String,
    /// 本 Node 发布的 topic 列表
    pub published_topics: Vec<String>,
    /// 本 Node 的数据面 REP 地址（Service 提供者需要）
    pub data_rep_addr: Option<String>,
    /// 本 Node 提供的 Service 名称（Service 提供者需要）
    pub service_name: Option<String>,
}

impl NodeTransport {
    /// 连接到 Kernel 消息总线
    ///
    /// 创建 DEALER + SUB socket，连接到 Kernel，发送注册消息。
    /// 返回传输层实例，通过通道接口收发消息。
    pub async fn connect(info: &NodeConnectInfo) -> anyhow::Result<Self> {
        // 1. 创建 DEALER socket 并连接到 Kernel ROUTER
        let mut dealer = DealerSocket::new();
        dealer.connect(&info.router_addr).await?;
        tracing::info!("DEALER socket 已连接：{}", info.router_addr);

        // 2. 创建 SUB socket 并连接到 Kernel PUB
        let mut subscriber = SubSocket::new();
        subscriber.connect(&info.pub_addr).await?;
        // 订阅空字符串前缀（接收所有广播消息）
        subscriber.subscribe("").await?;
        // 额外订阅 Node 感兴趣的 topic 前缀
        for sub in &info.subscriptions {
            subscriber.subscribe(sub.as_str()).await?;
        }
        tracing::info!("SUB socket 已连接：{} (订阅 {} 个 topic)", info.pub_addr, info.subscriptions.len());

        // 3. 创建通道和序列号管理器
        let (incoming_tx, incoming_rx) = mpsc::channel::<Message>(256);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Vec<Bytes>>(256);

        // 创建序列号管理器
        let sequence_manager = Arc::new(SequenceManager::new(info.node_id.clone()));

        let handle = NodeTransportHandle {
            outgoing_tx,
            sequence_manager: sequence_manager.clone(),
        };

        // 4. 发送 sys/register 消息（ROS 1 风格：包含数据面地址和正确的 node_type）
        let mut register_payload = serde_json::json!({
            "node_id": info.node_id.to_string(),
            "node_type": info.node_type,
            "subscriptions": info.subscriptions,
            "pub_addr": info.data_pub_addr,
            "published_topics": info.published_topics,
        });
        // 可选字段：Service 提供者的 REP 地址和服务名称
        if let Some(ref rep_addr) = info.data_rep_addr {
            register_payload["rep_addr"] = serde_json::Value::String(rep_addr.clone());
        }
        if let Some(ref service_name) = info.service_name {
            register_payload["service_name"] = serde_json::Value::String(service_name.clone());
        }

        let register_msg = FrameCodec::new_message_with_sequence(
            crate::message::Topic::sys_register(),
            info.node_id.as_str(),
            &register_payload,
            0,
        )?;
        let register_frames: Vec<Bytes> = FrameCodec::encode(&register_msg)?
            .into_iter()
            .map(Bytes::from)
            .collect();
        // 通过 outgoing 通道发送注册消息
        let init_tx = handle.outgoing_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Err(e) = init_tx.send(register_frames).await {
                tracing::warn!("注册消息发送失败：{}", e);
            }
        });

        // 5. 启动 DEALER 收发任务（zeromq 0.4 不支持 split，用 select! 交替收发）
        let dealer_handle = tokio::spawn(Self::dealer_loop(dealer, incoming_tx.clone(), outgoing_rx));

        // 6. 启动 SUB 接收任务
        let sub_recv_handle = tokio::spawn(Self::sub_recv_loop(subscriber, incoming_tx));

        Ok(Self {
            incoming_rx,
            handle,
            tasks: vec![dealer_handle, sub_recv_handle],
        })
    }

    /// 获取发送句柄
    pub fn handle(&self) -> &NodeTransportHandle {
        &self.handle
    }

    /// 接收下一条消息
    pub async fn recv(&mut self) -> anyhow::Result<Option<Message>> {
        Ok(self.incoming_rx.recv().await)
    }

    /// 优雅关闭传输层
    pub async fn shutdown(self) {
        drop(self.handle);
        drop(self.incoming_rx);
        for handle in self.tasks {
            let _ = handle.await;
        }
        tracing::debug!("Node 传输层已关闭");
    }

    // ---- 后台任务 ----

    /// DEALER 收发循环（zeromq 0.4 适配：使用 select! 交替收发）
    async fn dealer_loop(
        mut dealer: DealerSocket,
        tx: mpsc::Sender<Message>,
        mut outgoing_rx: mpsc::Receiver<Vec<Bytes>>,
    ) {
        loop {
            tokio::select! {
                // 优先处理发送
                frames_opt = outgoing_rx.recv() => {
                    match frames_opt {
                        Some(frames) => {
                            match ZmqMessage::try_from(frames) {
                                Ok(msg) => {
                                    if let Err(e) = dealer.send(msg).await {
                                        tracing::warn!("DEALER 发送失败：{}", e);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("DEALER 消息构造失败：{}", e);
                                }
                            }
                        }
                        None => {
                            tracing::debug!("DEALER 发送通道已关闭");
                            break;
                        }
                    }
                }
                // 接收消息
                zmq_result = dealer.recv() => {
                    match zmq_result {
                        Ok(zmq_msg) => {
                            // DEALER 收到：[topic, header, payload]（无 identity 帧前缀）
                            if zmq_msg.len() < 3 {
                                tracing::warn!("DEALER 收到帧数不足的消息：{} 帧", zmq_msg.len());
                                continue;
                            }
                            let frames: Vec<Vec<u8>> = zmq_msg
                                .iter()
                                .map(|b| b.to_vec())
                                .collect();

                            match FrameCodec::decode(&frames) {
                                Ok(message) => {
                                    if tx.send(message).await.is_err() {
                                        break;
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("DEALER 消息解码失败：{}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("DEALER 接收错误：{}", e);
                            break;
                        }
                    }
                }
            }
        }
        tracing::debug!("DEALER 收发循环退出");
    }

    /// SUB 接收循环
    async fn sub_recv_loop(mut subscriber: SubSocket, tx: mpsc::Sender<Message>) {
        loop {
            match subscriber.recv().await {
                Ok(zmq_msg) => {
                    if zmq_msg.len() < 3 {
                        tracing::warn!("SUB 收到帧数不足的消息：{} 帧", zmq_msg.len());
                        continue;
                    }
                    let frames: Vec<Vec<u8>> = zmq_msg
                        .iter()
                        .map(|b| b.to_vec())
                        .collect();

                    match FrameCodec::decode(&frames) {
                        Ok(message) => {
                            if tx.send(message).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("SUB 消息解码失败：{}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("SUB 接收错误：{}", e);
                    break;
                }
            }
        }
        tracing::debug!("SUB 接收循环退出");
    }
}

/// Node 消息循环入口
///
/// 连接消息总线，启动 Node，进入消息收发循环（含心跳）。
/// 这是每个 Node 运行的标准主循环。
pub async fn run_node<N: crate::node::Node + Send + 'static>(
    mut node: N,
    ctx: crate::node::NodeContext,
) -> anyhow::Result<()> {
    let node_id = node.node_id().clone();
    let subscriptions = node.subscriptions();

    // 1. 构建连接信息
    let connect_info = NodeConnectInfo {
        router_addr: ctx.router_addr.clone(),
        pub_addr: ctx.pub_addr.clone(),
        node_id: node_id.clone(),
        node_type: node.node_type().as_str().to_string(),
        subscriptions,
        data_pub_addr: ctx.data_pub_addr.clone(),
        published_topics: Vec::new(), // 由具体 Node 实现
        data_rep_addr: ctx.data_rep_addr.clone(),
        service_name: None, // 由具体 Node 实现
    };

    // 2. 连接消息总线
    let mut transport = NodeTransport::connect(&connect_info).await?;

    // 3. 将传输层句柄传给 Node
    let handle = transport.handle().clone();

    // 4. 启动 Node
    node.start(ctx).await?;
    tracing::info!("Node {} 已启动，进入消息循环", node_id);

    // 5. 心跳定时器
    let heartbeat_interval = std::time::Duration::from_secs(10);
    let mut heartbeat_timer = tokio::time::interval(heartbeat_interval);

    // 6. 数据面状态 — 缓存 publisher_discovery 发现的 Publisher 信息
    let mut data_plane = DataPlaneState::new();

    // 7. 消息循环（含心跳）
    loop {
        tokio::select! {
            // 接收消息
            msg_result = transport.recv() => {
                match msg_result {
                    Ok(Some(msg)) => {
                        // 系统消息特殊处理
                        if msg.topic.as_str() == "sys/shutdown" {
                            tracing::info!("Node {} 收到关闭信号", node_id);
                            break;
                        }
                        // 心跳响应（忽略）
                        if msg.topic.as_str() == "sys/heartbeat" {
                            continue;
                        }
                        // 注册响应 — publisher_discovery（ROS 1 数据面发现）
                        if msg.topic.as_str() == "sys/register" && msg.header.reply_to.is_some() {
                            match FrameCodec::decode_payload::<serde_json::Value>(&msg) {
                                Ok(payload) => {
                                    if payload.get("type").and_then(|v| v.as_str()) == Some("publisher_discovery") {
                                        data_plane.handle_discovery(&payload);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Node {} 解析 publisher_discovery 响应失败：{}", node_id, e);
                                }
                            }
                            continue;
                        }
                        // 业务消息交给 Node 处理
                        node.handle_message(msg, &handle).await?;
                    }
                    Ok(None) => {
                        tracing::warn!("Node {} 传输层通道已关闭", node_id);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Node {} 传输层接收错误：{}", node_id, e);
                        break;
                    }
                }
            }
            // 定期发送心跳
            _ = heartbeat_timer.tick() => {
                let heartbeat_msg = FrameCodec::new_message(
                    crate::message::Topic::sys_heartbeat(),
                    node_id.as_str(),
                    &serde_json::json!({ "node_id": node_id.to_string() }),
                )?;
                if let Err(e) = handle.send_message(&heartbeat_msg).await {
                    tracing::warn!("Node {} 心跳发送失败：{}", node_id, e);
                }
            }
        }
    }

    // 8. 停止 Node
    node.stop().await?;
    transport.shutdown().await;
    tracing::info!("Node {} 已停止", node_id);

    Ok(())
}
