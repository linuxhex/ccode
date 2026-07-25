//! Kernel 端 ZMQ 传输层
//!
//! 管理 ROUTER 和 PUB socket 的实际 ZMQ 连接，提供异步通道接口。
//!
//! 架构设计：
//! - ROUTER socket：接收所有 Node 的 DEALER 连接，定向转发消息
//! - PUB socket：广播系统事件（sys/shutdown, sys/spawn 等）
//!
//! 传输层将 ZMQ I/O 与业务逻辑解耦：
//! - 内部使用 tokio mpsc 通道传递消息
//! - ZMQ socket 操作在独立的后台任务中执行
//! - Broker 通过通道收发消息，无需直接操作 ZMQ socket
//!
//! ROUTER socket 消息格式：
//! - 接收：[identity, topic, header, payload] — 首帧是发送方 ZMQ identity
//! - 发送：[identity, topic, header, payload] — 首帧是目标 ZMQ identity

use bytes::Bytes;
use std::collections::HashMap;
use tokio::sync::mpsc;
use zeromq::{PubSocket, RouterRecvHalf, RouterSendHalf, RouterSocket, Socket, SocketRecv, SocketSend, ZmqMessage};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::node::NodeId;

/// Kernel 端 ZMQ 传输层
///
/// 封装 ROUTER + PUB socket 的异步 I/O，提供通道接口供 Broker 使用。
pub struct KernelTransport {
    /// ROUTER 接收通道 — 后台 recv 任务 → Broker
    incoming_rx: mpsc::Receiver<IncomingMessage>,
    /// ROUTER 发送通道 — Broker → 后台 send 任务
    router_send_tx: mpsc::Sender<RouterSendCommand>,
    /// PUB 广播通道 — Broker → 后台 pub 任务
    pub_send_tx: mpsc::Sender<Vec<Bytes>>,
    /// 后台任务 JoinHandle（用于优雅关闭）
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

/// 从 ROUTER 收到的消息
#[derive(Debug)]
pub struct IncomingMessage {
    /// 发送方的 ZMQ identity（用于回复时指定目标）
    pub identity: Bytes,
    /// 解码后的 ccode Message
    pub message: Message,
}

/// ROUTER 发送命令
#[derive(Debug)]
pub enum RouterSendCommand {
    /// 向指定 identity 发送消息帧
    Send {
        /// 目标 Node 的 ZMQ identity
        identity: Bytes,
        /// 消息帧 [topic, header, payload]
        frames: Vec<Bytes>,
    },
    /// 批量发送（向多个 identity 发送相同内容）
    MultiSend {
        /// 目标 identity 列表
        identities: Vec<Bytes>,
        /// 消息帧 [topic, header, payload]
        frames: Vec<Bytes>,
    },
    /// 关闭发送任务
    Shutdown,
}

impl KernelTransport {
    /// 创建并启动 Kernel 传输层
    ///
    /// 绑定 ROUTER 和 PUB socket，启动后台收发任务。
    /// 返回传输层实例，通过通道接口进行消息收发。
    pub async fn new(router_addr: &str, pub_addr: &str) -> anyhow::Result<Self> {
        // 1. 创建并绑定 ROUTER socket
        let mut router = RouterSocket::new();
        router.bind(router_addr).await?;
        tracing::info!("ROUTER socket 已绑定：{}", router_addr);

        // 2. 创建并绑定 PUB socket
        let mut publisher = PubSocket::new();
        publisher.bind(pub_addr).await?;
        tracing::info!("PUB socket 已绑定：{}", pub_addr);

        // 3. 拆分 ROUTER 为收发两半，实现并发
        let (router_send, router_recv) = router.split();

        // 4. 创建通道
        let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingMessage>(256);
        let (router_send_tx, router_send_rx) = mpsc::channel::<RouterSendCommand>(256);
        let (pub_send_tx, pub_send_rx) = mpsc::channel::<Vec<Bytes>>(64);

        // 5. 启动 ROUTER 接收任务
        let recv_handle = tokio::spawn(Self::router_recv_loop(router_recv, incoming_tx));

        // 6. 启动 ROUTER 发送任务
        let send_handle = tokio::spawn(Self::router_send_loop(router_send, router_send_rx));

        // 7. 启动 PUB 广播任务
        let pub_handle = tokio::spawn(Self::pub_loop(publisher, pub_send_rx));

        Ok(Self {
            incoming_rx,
            router_send_tx,
            pub_send_tx,
            tasks: vec![recv_handle, send_handle, pub_handle],
        })
    }

    /// 接收下一条来自 ROUTER 的消息
    ///
    /// 阻塞直到有消息到达或传输层关闭。
    pub async fn recv(&mut self) -> anyhow::Result<Option<IncomingMessage>> {
        Ok(self.incoming_rx.recv().await)
    }

    /// 通过 ROUTER 向指定 Node 发送消息
    pub async fn send_to(&self, identity: Bytes, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.router_send_tx
            .send(RouterSendCommand::Send { identity, frames })
            .await
            .map_err(|_| anyhow::anyhow!("ROUTER 发送通道已关闭"))
    }

    /// 通过 ROUTER 向多个 Node 广播同一消息
    pub async fn send_to_many(&self, identities: Vec<Bytes>, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.router_send_tx
            .send(RouterSendCommand::MultiSend { identities, frames })
            .await
            .map_err(|_| anyhow::anyhow!("ROUTER 发送通道已关闭"))
    }

    /// 通过 PUB socket 广播消息
    pub async fn broadcast(&self, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.pub_send_tx
            .send(frames)
            .await
            .map_err(|_| anyhow::anyhow!("PUB 广播通道已关闭"))
    }

    /// 优雅关闭传输层
    pub async fn shutdown(self) {
        // 关闭发送通道，触发后台任务退出
        drop(self.router_send_tx);
        drop(self.pub_send_tx);
        drop(self.incoming_rx);

        // 等待后台任务完成
        for handle in self.tasks {
            let _ = handle.await;
        }
        tracing::info!("Kernel 传输层已关闭");
    }

    // ---- 后台任务 ----

    /// ROUTER 接收循环
    ///
    /// 从 ROUTER socket 读取消息，解码后通过通道发送给 Broker。
    /// ROUTER 收到的消息首帧是发送方的 ZMQ identity。
    async fn router_recv_loop(mut recv_half: RouterRecvHalf, tx: mpsc::Sender<IncomingMessage>) {
        loop {
            match recv_half.recv().await {
                Ok(zmq_msg) => {
                    // 解析 ZMQ 消息：[identity, topic, header, payload]
                    let frame_count = zmq_msg.len();
                    if frame_count < 4 {
                        tracing::warn!("ROUTER 收到帧数不足的消息：{} 帧（期望 >= 4）", frame_count);
                        continue;
                    }

                    let identity = zmq_msg.get(0).cloned().unwrap_or_default();
                    let topic_bytes = zmq_msg.get(1).cloned().unwrap_or_default();
                    let header_bytes = zmq_msg.get(2).cloned().unwrap_or_default();
                    let payload_bytes = zmq_msg.get(3).cloned().unwrap_or_default();

                    // 解码 3 帧 Message
                    let frames = vec![
                        topic_bytes.to_vec(),
                        header_bytes.to_vec(),
                        payload_bytes.to_vec(),
                    ];

                    match FrameCodec::decode(&frames) {
                        Ok(message) => {
                            let incoming = IncomingMessage { identity, message };
                            if tx.send(incoming).await.is_err() {
                                break; // Broker 已关闭
                            }
                        }
                        Err(e) => {
                            tracing::warn!("消息解码失败：{}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("ROUTER 接收错误：{}", e);
                    break;
                }
            }
        }
        tracing::debug!("ROUTER 接收循环退出");
    }

    /// ROUTER 发送循环
    ///
    /// 从通道读取发送命令，通过 ROUTER socket 发送到目标 Node。
    /// ROUTER 发送消息首帧必须是目标的 ZMQ identity。
    async fn router_send_loop(mut send_half: RouterSendHalf, mut rx: mpsc::Receiver<RouterSendCommand>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                RouterSendCommand::Send { identity, frames } => {
                    // 构造 ZMQ 消息：[identity, topic, header, payload]
                    let mut zmq_frames: Vec<Bytes> = vec![identity];
                    zmq_frames.extend(frames);
                    let msg = ZmqMessage::from(zmq_frames);
                    if let Err(e) = send_half.send(msg).await {
                        tracing::warn!("ROUTER 发送失败：{}", e);
                    }
                }
                RouterSendCommand::MultiSend { identities, frames } => {
                    for identity in identities {
                        let mut zmq_frames: Vec<Bytes> = vec![identity];
                        zmq_frames.extend(frames.clone());
                        let msg = ZmqMessage::from(zmq_frames);
                        if let Err(e) = send_half.send(msg).await {
                            tracing::warn!("ROUTER 批量发送失败：{}", e);
                        }
                    }
                }
                RouterSendCommand::Shutdown => break,
            }
        }
        tracing::debug!("ROUTER 发送循环退出");
    }

    /// PUB 广播循环
    ///
    /// 从通道读取消息帧，通过 PUB socket 广播给所有订阅的 Node。
    /// PUB/SUB 模式：Node 的 SUB socket 订阅特定 topic 前缀。
    async fn pub_loop(mut publisher: PubSocket, mut rx: mpsc::Receiver<Vec<Bytes>>) {
        while let Some(frames) = rx.recv().await {
            let msg = ZmqMessage::from(frames);
            if let Err(e) = publisher.send(msg).await {
                tracing::warn!("PUB 广播失败：{}", e);
            }
        }
        // 关闭 publisher
        let _ = publisher.close().await;
        tracing::debug!("PUB 广播循环退出");
    }
}
