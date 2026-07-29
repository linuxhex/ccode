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
use zeromq::{DealerSocket, PubSocket, Socket, SocketRecv, SocketSend, SubSocket, ZmqMessage};

use crate::message::frame::FrameCodec;
use crate::message::{Message, SequenceManager};
use crate::node::NodeId;
use crate::performance::memory_pool::{BufferGuard, MessagePool};

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

/// 数据面状态 — 管理 Node 间 PUB/SUB 直连
///
/// ROS 1 风格：收到 publisher_discovery 后，自动创建 SUB socket 连接到发现的 Publisher，
/// 实现数据面直连（业务数据不经 Kernel）。
///
/// 每个 SUB socket 在独立 tokio task 中接收消息，转发到统一的 incoming 通道。
pub struct DataPlaneState {
    /// 已发现的 publisher：pub_addr → PublisherInfo
    publishers: HashMap<String, PublisherInfo>,
    /// 已建立的 SUB 连接：pub_addr → JoinHandle
    sub_handles: HashMap<String, tokio::task::JoinHandle<()>>,
    /// 本 Node 订阅的 topic 模式列表（用于匹配 Publisher 的 topic）
    local_subscriptions: Vec<String>,
}

impl DataPlaneState {
    /// 创建数据面状态
    pub fn new(local_subscriptions: Vec<String>) -> Self {
        Self {
            publishers: HashMap::new(),
            sub_handles: HashMap::new(),
            local_subscriptions,
        }
    }

    /// 处理 Kernel 返回的 publisher_discovery 响应
    ///
    /// 解析响应中的 publisher 列表，缓存到本地状态。
    /// 对新发现的 Publisher，自动创建 SUB socket 并订阅匹配的 topic。
    pub fn handle_discovery(&mut self, payload: &serde_json::Value, incoming_tx: mpsc::Sender<Message>) {
        let publishers = match payload.get("publishers").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return,
        };

        for entry in publishers {
            if let Some(pub_list) = entry.get("publishers").and_then(|v| v.as_array()) {
                for p in pub_list {
                    let node_id = p["node_id"].as_str().unwrap_or("").to_string();
                    let pub_addr = p["pub_addr"].as_str().unwrap_or("").to_string();
                    let topics: Vec<String> = p["topics"]
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

                    // 如果已有连接，跳过
                    if self.sub_handles.contains_key(&pub_addr) {
                        continue;
                    }

                    tracing::info!(
                        "发现新 Publisher：node={}, addr={}, topics={:?}",
                        node_id, pub_addr, topics
                    );

                    // 计算本 Node 需要订阅此 Publisher 的哪些 topic
                    // 策略：通配符模式（含 *）不走数据面直连（ZMQ SUB 不支持通配符），
                    // 由 Kernel 控制面路由负责转发。仅精确 topic 走数据面直连。
                    let exact_topics: Vec<String> = topics
                        .iter()
                        .filter(|t| !t.contains('*'))
                        .filter(|t| Self::topic_matches(&self.local_subscriptions, t))
                        .cloned()
                        .collect();

                    if exact_topics.is_empty() {
                        tracing::debug!(
                            "Publisher {} 无精确匹配 topic（通配符模式由控制面路由），跳过数据面 SUB 连接",
                            pub_addr
                        );
                        self.publishers.insert(
                            pub_addr.clone(),
                            PublisherInfo { node_id, pub_addr, topics },
                        );
                        continue;
                    }

                    // 缓存 Publisher 信息
                    self.publishers.insert(
                        pub_addr.clone(),
                        PublisherInfo { node_id: node_id.clone(), pub_addr: pub_addr.clone(), topics },
                    );

                    // 启动 SUB 连接任务（仅精确 topic，ZMQ SUB 前缀匹配即可正确过滤）
                    let handle = tokio::spawn(Self::sub_connect_loop(
                        pub_addr.clone(),
                        exact_topics,
                        incoming_tx.clone(),
                    ));
                    self.sub_handles.insert(pub_addr, handle);
                }
            }
        }
    }

    /// 处理 publisher_change 通知（新 Publisher 上线时 Kernel 推送）
    ///
    /// 与 handle_discovery 逻辑相同，解析并连接新 Publisher。
    pub fn handle_publisher_change(&mut self, payload: &serde_json::Value, incoming_tx: mpsc::Sender<Message>) {
        // publisher_change 格式与 publisher_discovery 相同，复用解析逻辑
        self.handle_discovery(payload, incoming_tx);
    }

    /// 判断 topic 是否匹配本 Node 的订阅模式
    fn topic_matches(subscriptions: &[String], topic: &str) -> bool {
        // 精确匹配或前缀匹配
        subscriptions.iter().any(|sub| {
            topic == sub || topic.starts_with(sub)
        })
    }

    /// 将通配符订阅模式转换为 ZMQ SUB 前缀
    ///
    /// ZMQ SUB 的 subscribe() 只支持字符串前缀匹配，不支持通配符。
    /// 因此 "sampler/*/stream" 不能直接用于 ZMQ 订阅。
    /// 转换策略：取通配符出现前的最长前缀作为 ZMQ 订阅前缀。
    ///   "sampler/*/stream" → "sampler/"
    ///   "agent/*/tool_call" → "agent/"
    ///   "sys/shutdown" → "sys/shutdown"（无通配符，原样返回）
    fn zmq_subscribe_prefix(pattern: &str) -> &str {
        // 找到第一个通配符（* 或 **）的位置
        if let Some(pos) = pattern.find('*') {
            // 取通配符前最后一段的前缀（截断到最后一个 /）
            let prefix = &pattern[..pos];
            if let Some(slash_pos) = prefix.rfind('/') {
                &pattern[..=slash_pos]  // "sampler/*" → "sampler/"
            } else {
                prefix  // 无 / 的情况，如 "*/xxx" → ""
            }
        } else {
            pattern  // 无通配符，原样返回
        }
    }

    /// 启动 SUB 连接循环
    ///
    /// 创建 SUB socket 连接到指定 Publisher，订阅匹配的 topic，
    /// 接收消息后转发到 incoming 通道。
    async fn sub_connect_loop(
        pub_addr: String,
        topics: Vec<String>,
        tx: mpsc::Sender<Message>,
    ) {
        tracing::info!("正在连接 Publisher SUB：{}", pub_addr);

        let mut subscriber = SubSocket::new();
        match subscriber.connect(&pub_addr).await {
            Ok(()) => {
                tracing::info!("SUB socket 已连接到 Publisher：{}", pub_addr);
            }
            Err(e) => {
                tracing::error!("SUB socket 连接失败 {}：{}", pub_addr, e);
                return;
            }
        }

        // 订阅匹配的 topic（使用 ZMQ 前缀订阅，因 ZMQ 不支持通配符）
        for topic in &topics {
            let zmq_prefix = Self::zmq_subscribe_prefix(topic);
            if let Err(e) = subscriber.subscribe(zmq_prefix).await {
                tracing::warn!("SUB 订阅 topic {} (prefix={}) 失败：{}", topic, zmq_prefix, e);
            } else {
                tracing::debug!("SUB 订阅 ZMQ 前缀：{} (原始模式：{})", zmq_prefix, topic);
            }
        }

        // 接收循环
        loop {
            match subscriber.recv().await {
                Ok(zmq_msg) => {
                    if zmq_msg.len() < 3 {
                        tracing::warn!("数据面 SUB 收到帧数不足的消息：{} 帧", zmq_msg.len());
                        continue;
                    }
                    let frames: Vec<Vec<u8>> = zmq_msg.iter().map(|b| b.to_vec()).collect();

                    match FrameCodec::decode(&frames) {
                        Ok(message) => {
                            if tx.send(message).await.is_err() {
                                break; // 通道已关闭
                            }
                        }
                        Err(e) => {
                            tracing::warn!("数据面 SUB 消息解码失败：{}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("数据面 SUB 接收错误 {}：{}", pub_addr, e);
                    break;
                }
            }
        }
        tracing::debug!("数据面 SUB 连接循环退出：{}", pub_addr);
    }

    /// 获取所有已发现的 publisher 信息
    pub fn publishers(&self) -> &HashMap<String, PublisherInfo> {
        &self.publishers
    }
}

/// Node 端传输层句柄
///
/// Node 通过此句柄发送消息到消息总线。
/// 支持控制面（DEALER→ROUTER）和数据面（PUB→SUB）两种发送路径。
#[derive(Clone)]
pub struct NodeTransportHandle {
    /// 控制面发送通道（DEALER → Kernel ROUTER）
    outgoing_tx: mpsc::Sender<Vec<Bytes>>,
    /// 数据面发布通道（PUB → 其他 Node SUB）
    data_pub_tx: mpsc::Sender<Vec<Bytes>>,
    /// 消息接收通道（用于 DataPlaneState 的 SUB 连接转发消息）
    incoming_tx: mpsc::Sender<Message>,
    /// 序列号管理器
    sequence_manager: Arc<SequenceManager>,
    /// 消息内存池 — 池化序列化缓冲区，减少消息发送时的内存分配
    message_pool: Arc<MessagePool>,
}

impl NodeTransportHandle {
    /// 发送消息到消息总线（自动添加序列号）
    ///
    /// 将 ccode Message 编码为 3 帧格式（topic + header + payload），
    /// 通过 DEALER socket 发送到 Kernel ROUTER。
    pub async fn send_message(&self, msg: &Message) -> anyhow::Result<()> {
        // 获取下一个序列号
        let sequence = self.sequence_manager.next_sequence();

        // 创建带序列号的消息（保留原 requires_ack 标志，确保关键控制面消息的可靠性传递）
        let msg_with_seq = Message {
            topic: msg.topic.clone(),
            header: crate::message::MessageHeader {
                msg_id: msg.header.msg_id.clone(),
                timestamp: msg.header.timestamp.clone(),
                src_node: msg.header.src_node.clone(),
                reply_to: msg.header.reply_to.clone(),
                sequence,
                requires_ack: msg.header.requires_ack,
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

    /// 通过数据面 PUB socket 发布消息（Node 间直连，不经 Kernel）
    ///
    /// 将 ccode Message 编码后通过 PUB socket 广播，
    /// 订阅了对应 topic 的 Node 会直接收到。
    ///
    /// 使用消息内存池获取池化缓冲区进行序列化，避免每次发送都重新分配工作缓冲区。
    /// BufferGuard drop 时自动归还缓冲区到池中复用；Bytes 拷贝自缓冲区独立持有数据，
    /// 不共享池化缓冲区的底层分配，避免异步发送场景下的数据竞争。
    pub async fn publish_data(&self, msg: &Message) -> anyhow::Result<()> {
        let frames = FrameCodec::encode(msg)?;

        // 从内存池获取缓冲区（优先复用池中已有缓冲区，减少工作缓冲区分配）
        let mut guard = BufferGuard::new(&*self.message_pool);
        let buf = guard.as_mut();

        // 将所有帧连续写入池化缓冲区，记录每帧偏移量
        let mut offsets: Vec<usize> = Vec::with_capacity(frames.len() + 1);
        offsets.push(0);
        for frame in &frames {
            buf.extend_from_slice(frame);
            offsets.push(buf.len());
        }

        // 从池化缓冲区切片拷贝创建 Bytes（独立所有权，缓冲区可安全归还复用）
        let mut bytes_frames: Vec<Bytes> = Vec::with_capacity(frames.len());
        for w in offsets.windows(2) {
            bytes_frames.push(Bytes::copy_from_slice(&buf[w[0]..w[1]]));
        }
        // guard drop 时自动归还缓冲区到池中复用

        self.data_pub_tx
            .send(bytes_frames)
            .await
            .map_err(|_| anyhow::anyhow!("数据面发布通道已关闭"))
    }

    /// 通过数据面 PUB socket 发布原始帧
    pub async fn publish_frames(&self, frames: Vec<Bytes>) -> anyhow::Result<()> {
        self.data_pub_tx
            .send(frames)
            .await
            .map_err(|_| anyhow::anyhow!("数据面发布通道已关闭"))
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

        // 2. 创建 SUB socket 并连接到 Kernel PUB（控制面广播）
        let mut subscriber = SubSocket::new();
        subscriber.connect(&info.pub_addr).await?;
        // 订阅空字符串前缀（接收所有广播消息）
        subscriber.subscribe("").await?;
        // 额外订阅 Node 感兴趣的 topic 前缀
        for sub in &info.subscriptions {
            subscriber.subscribe(sub.as_str()).await?;
        }
        tracing::info!("SUB socket 已连接：{} (订阅 {} 个 topic)", info.pub_addr, info.subscriptions.len());

        // 3. 创建数据面 PUB socket（如果配置了 data_pub_addr）
        let has_pub_socket = !info.data_pub_addr.is_empty();
        if has_pub_socket {
            tracing::info!("数据面 PUB socket 将绑定到：{}", info.data_pub_addr);
        }

        // 4. 创建通道和序列号管理器
        let (incoming_tx, incoming_rx) = mpsc::channel::<Message>(256);
        let (outgoing_tx, outgoing_rx) = mpsc::channel::<Vec<Bytes>>(256);
        let (data_pub_tx, data_pub_rx) = mpsc::channel::<Vec<Bytes>>(64);

        // 创建序列号管理器
        let sequence_manager = Arc::new(SequenceManager::new(info.node_id.clone()));

        // 创建消息内存池（64 个 64KB 缓冲区，供数据面序列化复用）
        let message_pool = Arc::new(MessagePool::new(64, 64 * 1024));

        let handle = NodeTransportHandle {
            outgoing_tx,
            data_pub_tx,
            incoming_tx: incoming_tx.clone(),
            sequence_manager: sequence_manager.clone(),
            message_pool,
        };

        // 5. 发送 sys/register 消息（ROS 1 风格：包含数据面地址和正确的 node_type）
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

        // 6. 启动 DEALER 收发任务（zeromq 0.4 不支持 split，用 select! 交替收发）
        let dealer_handle = tokio::spawn(Self::dealer_loop(dealer, incoming_tx.clone(), outgoing_rx));

        // 7. 启动 SUB 接收任务（控制面广播）
        let sub_recv_handle = tokio::spawn(Self::sub_recv_loop(subscriber, incoming_tx));

        // 8. 启动数据面 PUB 发布任务（如果配置了 PUB 地址）
        let mut tasks = vec![dealer_handle, sub_recv_handle];
        if has_pub_socket {
            let pub_addr = info.data_pub_addr.clone();
            let pub_handle = tokio::spawn(Self::data_pub_loop(pub_addr, data_pub_rx));
            tasks.push(pub_handle);
        } else {
            // 没有 PUB socket 时，丢弃发布请求
            drop(data_pub_rx);
        }

        Ok(Self {
            incoming_rx,
            handle,
            tasks,
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
            if let Err(e) = handle.await {
                tracing::debug!("Node 后台任务退出异常：{}", e);
            }
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

    /// 数据面 PUB 发布循环
    ///
    /// 绑定 PUB socket，从通道读取消息帧并广播。
    /// 订阅了对应 topic 的 Node 会通过数据面 SUB 直连收到。
    async fn data_pub_loop(pub_addr: String, mut rx: mpsc::Receiver<Vec<Bytes>>) {
        let mut publisher = PubSocket::new();
        match publisher.bind(&pub_addr).await {
            Ok(_endpoint) => {
                tracing::info!("数据面 PUB socket 已绑定：{}", pub_addr);
            }
            Err(e) => {
                tracing::error!("数据面 PUB socket 绑定失败 {}：{}", pub_addr, e);
                return;
            }
        }

        while let Some(frames) = rx.recv().await {
            match ZmqMessage::try_from(frames) {
                Ok(msg) => {
                    if let Err(e) = publisher.send(msg).await {
                        tracing::warn!("数据面 PUB 发布失败：{}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("数据面 PUB 消息构造失败：{}", e);
                }
            }
        }
        let errors = publisher.close().await;
        if !errors.is_empty() {
            tracing::debug!("数据面 PUB socket 关闭失败：{:?}", errors);
        }
        tracing::debug!("数据面 PUB 发布循环退出");
    }
}

/// Node 消息循环入口
///
/// 连接消息总线，启动 Node，进入消息收发循环（含心跳）。
/// 这是每个 Node 运行的标准主循环。
///
/// ACK 机制：心跳等关键控制面消息发送后等待 Kernel 的 ACK 确认，
/// 超时未确认则自动重传（最多 3 次，指数退避）。
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
        subscriptions: subscriptions.clone(),
        data_pub_addr: ctx.data_pub_addr.clone(),
        published_topics: node.published_topics(),
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

    // 6. 数据面状态 — 管理 Node 间 PUB/SUB 直连
    let mut data_plane = DataPlaneState::new(subscriptions);

    // 7. ACK 确认管理器 — 关键控制面消息的可靠性保障
    let ack_manager = std::sync::Arc::new(
        crate::message::ack::AckManager::new(crate::message::ack::AckConfig::default())
    );
    let (retry_tx, mut retry_rx) = mpsc::channel::<crate::message::Message>(64);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
    // 启动重试后台任务：超时未确认的消息通过 retry_tx 回传，由主循环重发
    let ack_for_retry = ack_manager.clone();
    tokio::spawn(crate::message::ack::retry_loop(ack_for_retry, retry_tx, shutdown_rx));

    // 8. 消息循环（含心跳 + 数据面发现 + ACK 处理）
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
                        // ACK 确认消息 — 关联已发送消息，标记为已确认
                        if msg.topic.as_str() == "sys/ack" {
                            if let Some(reply_to) = &msg.header.reply_to {
                                let acked = ack_manager.handle_ack(reply_to).await;
                                if acked {
                                    tracing::debug!("Node {} 收到 ACK：{}", node_id, reply_to);
                                }
                            }
                            continue;
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
                                        tracing::info!("Node {} 收到 publisher_discovery", node_id);
                                        data_plane.handle_discovery(&payload, handle.incoming_tx.clone());
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("Node {} 解析 publisher_discovery 响应失败：{}", node_id, e);
                                }
                            }
                            continue;
                        }
                        // publisher_change 通知 — 新 Publisher 上线
                        if msg.topic.as_str() == "sys/publisher_change" {
                            match FrameCodec::decode_payload::<serde_json::Value>(&msg) {
                                Ok(payload) => {
                                    tracing::info!("Node {} 收到 publisher_change 通知", node_id);
                                    data_plane.handle_publisher_change(&payload, handle.incoming_tx.clone());
                                }
                                Err(e) => {
                                    tracing::warn!("Node {} 解析 publisher_change 失败：{}", node_id, e);
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
            // 定期发送心跳（requires_ack=true，确保 Kernel 收到）
            _ = heartbeat_timer.tick() => {
                let mut heartbeat_msg = FrameCodec::new_message(
                    crate::message::Topic::sys_heartbeat(),
                    node_id.as_str(),
                    &serde_json::json!({ "node_id": node_id.to_string() }),
                )?;
                // 标记需要 ACK 确认，并记录到 AckManager 等待确认
                heartbeat_msg.header.requires_ack = true;
                let msg_id = heartbeat_msg.header.msg_id.clone();
                if let Err(e) = handle.send_message(&heartbeat_msg).await {
                    tracing::warn!("Node {} 心跳发送失败：{}", node_id, e);
                } else {
                    ack_manager.record_sent(heartbeat_msg).await;
                    tracing::trace!("Node {} 心跳已发送，等待 ACK：{}", node_id, msg_id);
                }
            }
            // 重试消息：AckManager 检测到超时未确认，通过 retry_tx 回传待重发消息
            retry_msg = retry_rx.recv() => {
                if let Some(msg) = retry_msg {
                    tracing::warn!("Node {} 重发超时消息：{}", node_id, msg.header.msg_id);
                    if let Err(e) = handle.send_message(&msg).await {
                        tracing::warn!("Node {} 重发失败：{}", node_id, e);
                    }
                }
            }
        }
    }

    // 9. 优雅停止 Node 和重试任务
    if let Err(e) = shutdown_tx.send(()).await {
        tracing::debug!("关闭信号发送失败：{}", e);
    }
    node.graceful_stop(Some(&handle)).await?;
    transport.shutdown().await;
    tracing::info!("Node {} 已停止", node_id);

    Ok(())
}
