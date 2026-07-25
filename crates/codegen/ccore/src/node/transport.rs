//! Node 端 ZMQ 传输层
//!
//! 每个 Node 进程通过此传输层连接 Kernel 的消息总线。
//!
//! 连接模型：
//! - DealerSocket → 连接 Kernel ROUTER（发送/接收定向消息）
//! - SubSocket → 连接 Kernel PUB（接收广播消息）
//!
//! 通道架构：
//! - incoming 通道：后台收任务 → Node 消息循环
//! - outgoing 通道：Node → 后台发任务 → DEALER socket
//!
//! 注册流程：
//! 1. Node 创建 DealerSocket + SubSocket
//! 2. 连接 Kernel 的 ROUTER 和 PUB 地址
//! 3. SubSocket 订阅相关 topic 前缀
//! 4. 通过 DEALER 发送 sys/register 消息（含 NodeId、NodeType、subscriptions）
//! 5. Kernel 收到后注册 identity 映射和订阅关系
//! 6. Node 进入消息循环，等待 incoming 通道的消息

use bytes::Bytes;
use tokio::sync::mpsc;
use zeromq::{DealerRecvHalf, DealerSendHalf, DealerSocket, Socket, SocketRecv, SubSocket, ZmqMessage};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::node::NodeId;

/// Node 端传输层句柄
///
/// Node 通过此句柄发送消息到消息总线。
/// 内部通过 mpsc 通道与后台 DEALER 发送任务通信。
#[derive(Clone)]
pub struct NodeTransportHandle {
    /// 发送消息的通道
    outgoing_tx: mpsc::Sender<Vec<Bytes>>,
}

impl NodeTransportHandle {
    /// 发送消息到消息总线
    ///
    /// 将 ccode Message 编码为 3 帧格式（topic + header + payload），
    /// 通过 DEALER socket 发送到 Kernel ROUTER。
    pub async fn send_message(&self, msg: &Message) -> anyhow::Result<()> {
        let frames = FrameCodec::encode(msg)?;
        let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
        self.outgoing_tx
            .send(bytes_frames)
            .await
            .map_err(|_| anyhow::anyhow!("发送通道已关闭"))
    }

    /// 发送原始帧到消息总线
    pub async fn send_frames(&self, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.outgoing_tx
            .send(frames)
            .await
            .map_err(|_| anyhow::anyhow!("发送通道已关闭"))
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

impl NodeTransport {
    /// 连接到 Kernel 消息总线
    ///
    /// 创建 DEALER + SUB socket，连接到 Kernel，发送注册消息。
    /// 返回传输层实例，通过通道接口收发消息。
    ///
    /// # 参数
    /// - `router_addr`: Kernel ROUTER socket 地址
    /// - `pub_addr`: Kernel PUB socket 地址
    /// - `node_id`: 本 Node 的唯一 ID
    /// - `subscriptions`: 需要订阅的 topic 模式列表
    pub async fn connect(
        router_addr: &str,
        pub_addr: &str,
        node_id: &NodeId,
        subscriptions: &[String],
    ) -> anyhow::Result<Self> {
        // 1. 创建 DEALER socket 并连接到 Kernel ROUTER
        let mut dealer = DealerSocket::new();
        dealer.connect(router_addr).await?;
        tracing::info!("DEALER socket 已连接：{}", router_addr);

        // 2. 创建 SUB socket 并连接到 Kernel PUB
        let mut subscriber = SubSocket::new();
        subscriber.connect(pub_addr).await?;
        // 订阅空字符串前缀（接收所有广播消息）
        subscriber.subscribe("").await?;
        // 额外订阅 Node 感兴趣的 topic 前缀
        for sub in subscriptions {
            subscriber.subscribe(sub.as_str()).await?;
        }
        tracing::info!("SUB socket 已连接：{} (订阅 {} 个 topic)", pub_addr, subscriptions.len());

        // 3. 拆分 DEALER 为收发两半
        let (dealer_send, dealer_recv) = dealer.split();

        // 4. 创建通道
        let (incoming_tx, incoming_rx) = mpsc::channel::<Message>(256);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Vec<Bytes>>(256);

        let handle = NodeTransportHandle { outgoing_tx };

        // 5. 发送 sys/register 消息
        let register_payload = serde_json::json!({
            "node_id": node_id.to_string(),
            "node_type": "agent", // 由调用方指定
            "subscriptions": subscriptions,
        });
        let register_msg = FrameCodec::new_message(
            crate::message::Topic::sys_register(),
            node_id.as_str(),
            &register_payload,
        )?;
        let register_frames: Vec<Bytes> = FrameCodec::encode(&register_msg)?
            .into_iter()
            .map(Bytes::from)
            .collect();
        // 立即通过 DEALER 发送注册消息（在启动后台任务前）
        // 因为后台任务还没启动，我们直接通过 dealer_send 发送
        // 但 dealer_send 是 DealerSendHalf，需要先启动后台任务
        // 所以我们先通过 outgoing 通道发送
        let init_tx = outgoing_tx.clone();
        tokio::spawn(async move {
            // 短暂延迟等待 DEALER 连接建立
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Err(e) = init_tx.send(register_frames).await {
                tracing::warn!("注册消息发送失败：{}", e);
            }
        });

        // 6. 启动 DEALER 接收任务
        let dealer_recv_handle = tokio::spawn(Self::dealer_recv_loop(dealer_recv, incoming_tx.clone()));

        // 7. 启动 SUB 接收任务
        let sub_recv_handle = tokio::spawn(Self::sub_recv_loop(subscriber, incoming_tx));

        // 8. 启动 DEALER 发送任务
        let dealer_send_handle = tokio::spawn(Self::dealer_send_loop(dealer_send, outgoing_rx));

        Ok(Self {
            incoming_rx,
            handle,
            tasks: vec![dealer_recv_handle, sub_recv_handle, dealer_send_handle],
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

    /// DEALER 接收循环
    ///
    /// 从 DEALER socket 接收来自 Kernel ROUTER 转发的定向消息。
    /// DEALER 收到的消息没有 identity 帧前缀（被 ROUTER 剥离）。
    async fn dealer_recv_loop(mut recv_half: DealerRecvHalf, tx: mpsc::Sender<Message>) {
        loop {
            match recv_half.recv().await {
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
        tracing::debug!("DEALER 接收循环退出");
    }

    /// SUB 接收循环
    ///
    /// 从 SUB socket 接收 Kernel PUB 广播的消息。
    /// PUB/SUB 消息格式：[topic_prefix, topic, header, payload] 或 [topic, header, payload]。
    async fn sub_recv_loop(mut subscriber: SubSocket, tx: mpsc::Sender<Message>) {
        loop {
            match subscriber.recv().await {
                Ok(zmq_msg) => {
                    // PUB/SUB 消息格式取决于订阅方式
                    // 如果订阅的是空字符串，收到的消息格式为 [topic, header, payload]
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

    /// DEALER 发送循环
    ///
    /// 从通道读取消息帧，通过 DEALER socket 发送到 Kernel ROUTER。
    async fn dealer_send_loop(mut send_half: DealerSendHalf, mut rx: mpsc::Receiver<Vec<Bytes>>) {
        while let Some(frames) = rx.recv().await {
            let msg = ZmqMessage::from(frames);
            if let Err(e) = send_half.send(msg).await {
                tracing::warn!("DEALER 发送失败：{}", e);
            }
        }
        tracing::debug!("DEALER 发送循环退出");
    }
}

/// Node 消息循环入口
///
/// 连接消息总线，启动 Node，进入消息收发循环。
/// 这是每个 Node 运行的标准主循环。
pub async fn run_node<N: crate::node::Node + Send + 'static>(
    mut node: N,
    ctx: crate::node::NodeContext,
) -> anyhow::Result<()> {
    let node_id = node.node_id().clone();
    let subscriptions = node.subscriptions();

    // 1. 连接消息总线
    let mut transport = NodeTransport::connect(
        &ctx.router_addr,
        &ctx.pub_addr,
        &node_id,
        &subscriptions,
    )
    .await?;

    // 2. 将传输层句柄传给 Node
    let handle = transport.handle().clone();

    // 3. 启动 Node
    node.start(ctx).await?;
    tracing::info!("Node {} 已启动，进入消息循环", node_id);

    // 4. 消息循环
    while let Some(msg) = transport.recv().await? {
        // 系统消息特殊处理
        if msg.topic.as_str() == "sys/shutdown" {
            tracing::info!("Node {} 收到关闭信号", node_id);
            break;
        }

        // 心跳响应
        if msg.topic.as_str() == "sys/heartbeat" {
            // 自动回复心跳，不需要 Node 处理
            continue;
        }

        // 业务消息交给 Node 处理
        node.handle_message(msg, &handle).await?;
    }

    // 5. 停止 Node
    node.stop().await?;
    transport.shutdown().await;
    tracing::info!("Node {} 已停止", node_id);

    Ok(())
}
