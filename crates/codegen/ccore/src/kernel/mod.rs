//! Kernel - 消息总线 broker，Node 注册/发现/健康检查
//!
//! Kernel 是 ccode 的微内核，负责：
//! 1. 运行 Broker（消息路由）
//! 2. 维护 Registry（Node 注册表）
//! 3. 执行健康检查（心跳超时检测）
//! 4. 管理 Node 生命周期（spawn/deregister）
//! 5. 启动初始 Node 集合
//!
//! 事件循环：
//! ```
//! Kernel 启动
//!   ├─ 创建 KernelTransport（绑定 ROUTER + PUB socket）
//!   ├─ 创建 NodeLauncher
//!   ├─ spawn 初始 Node 集合（各 Node 连接 DEALER + SUB 到 Kernel）
//!   └─ 进入事件循环：
//!       ├─ recv from ROUTER → 路由消息到订阅者
//!       ├─ sys/register → 注册 Node（identity + subscriptions）
//!       ├─ sys/heartbeat → 更新心跳时间戳
//!       ├─ agent/{id}/spawn → spawn 子 Agent
//!       └─ 定期健康检查 → 清理超时 Node
//! ```

pub mod broker;
pub mod registry;
pub mod health;
pub mod transport;
pub mod launcher;

use anyhow::Result;
use bytes::Bytes;
use std::time::Duration;

use crate::config::CcodeConfig;
use crate::kernel::transport::{IncomingMessage, KernelTransport, RouterSendCommand};
use crate::message::frame::FrameCodec;
use crate::message::Topic;
use crate::message::Message;
use crate::node::{NodeId, NodeType, NodeContext};

/// Kernel 配置
#[derive(Debug, Clone)]
pub struct KernelConfig {
    /// ROUTER socket 绑定地址
    pub router_addr: String,
    /// PUB socket 绑定地址
    pub pub_addr: String,
    /// 心跳超时（秒）
    pub heartbeat_timeout_secs: u64,
    /// 最大子 Agent 数量
    pub max_subagents: usize,
    /// 健康检查间隔（秒）
    pub health_check_interval_secs: u64,
    /// 工作目录
    pub working_dir: String,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            router_addr: "ipc:///tmp/ccode-router".into(),
            pub_addr: "ipc:///tmp/ccode-pub".into(),
            heartbeat_timeout_secs: 30,
            max_subagents: 10,
            health_check_interval_secs: 10,
            working_dir: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
        }
    }
}

/// Kernel 主结构
///
/// Kernel 持有：
/// - `transport`: ZMQ 传输层（ROUTER + PUB socket 的异步 I/O）
/// - `broker`: 消息路由逻辑（identity 映射、订阅关系、路由表）
/// - `registry`: Node 注册表（元数据、心跳时间戳）
/// - `ccode_config`: ccode 全局配置（供 Launcher 使用）
pub struct Kernel {
    config: KernelConfig,
    broker: broker::Broker,
    registry: registry::Registry,
    transport: Option<KernelTransport>,
    running: bool,
    ccode_config: Option<CcodeConfig>,
}

impl Kernel {
    pub fn new(config: KernelConfig) -> Self {
        let broker = broker::Broker::new(
            config.router_addr.clone(),
            config.pub_addr.clone(),
        );
        Self {
            config,
            broker,
            registry: registry::Registry::new(),
            transport: None,
            running: false,
            ccode_config: None,
        }
    }

    /// 设置 ccode 全局配置
    pub fn set_ccode_config(&mut self, config: CcodeConfig) {
        self.ccode_config = Some(config);
    }

    /// 获取 NodeContext 供子 Node 连接
    pub fn node_context(&self) -> NodeContext {
        NodeContext {
            router_addr: self.config.router_addr.clone(),
            pub_addr: self.config.pub_addr.clone(),
        }
    }

    /// 启动 Kernel 事件循环
    ///
    /// 主循环流程：
    /// 1. 启动 KernelTransport（绑定 ZMQ socket）
    /// 2. 进入事件循环：
    ///    - 接收 ROUTER 消息 → 处理系统消息或路由
    ///    - 定期健康检查
    ///    - 处理 Node spawn 请求
    pub async fn run(&mut self) -> Result<()> {
        self.running = true;

        tracing::info!(
            "Kernel 启动：router={}, pub={}, working_dir={}",
            self.config.router_addr,
            self.config.pub_addr,
            self.config.working_dir
        );

        // 1. 启动 KernelTransport
        let mut transport = KernelTransport::new(
            &self.config.router_addr,
            &self.config.pub_addr,
        ).await?;
        self.transport = Some(transport);

        let transport = self.transport.as_mut().unwrap();

        // 2. 启动初始 Node 集合
        if let Some(ccfg) = self.ccode_config.take() {
            let mut launcher = launcher::NodeLauncher::new(self.config.clone(), ccfg);
            match launcher.spawn_initial_set().await {
                Ok(nodes) => {
                    tracing::info!("初始 Node 集合启动完成：{} 个", nodes.len());
                }
                Err(e) => {
                    tracing::error!("初始 Node 集合启动失败：{}", e);
                }
            }
        } else {
            tracing::warn!("未设置 ccode_config，跳过初始 Node 集合启动");
        }

        // 3. 主事件循环
        let health_interval = Duration::from_secs(self.config.health_check_interval_secs);
        let mut health_timer = tokio::time::interval(health_interval);

        while self.running {
            tokio::select! {
                // 从 ROUTER 接收消息
                incoming = transport.recv() => {
                    match incoming {
                        Ok(Some(incoming)) => {
                            if let Err(e) = self.handle_incoming(incoming, transport).await {
                                tracing::warn!("处理消息失败：{}", e);
                            }
                        }
                        Ok(None) => {
                            tracing::warn!("传输层通道已关闭");
                            break;
                        }
                        Err(e) => {
                            tracing::error!("传输层接收错误：{}", e);
                        }
                    }
                }
                // 定期健康检查
                _ = health_timer.tick() => {
                    let dead_nodes = self.registry.remove_stale(self.config.heartbeat_timeout_secs);
                    for node_id in dead_nodes {
                        tracing::warn!("Node 心跳超时，移除：{}", node_id);
                        self.broker.deregister_identity(&node_id);
                        self.broadcast_node_deregister(transport, &node_id).await;
                    }
                }
            }
        }

        // 关闭流程
        self.broadcast_shutdown(transport).await;
        if let Some(t) = self.transport.take() {
            t.shutdown().await;
        }
        Ok(())
    }

    /// 处理从 ROUTER 收到的消息
    async fn handle_incoming(
        &mut self,
        incoming: IncomingMessage,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        let topic = incoming.message.topic.as_str();
        let identity = incoming.identity.clone();

        match topic {
            // 系统消息：Node 注册
            "sys/register" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_type_str = payload["node_type"].as_str().unwrap_or("agent");
                let node_id = NodeId::from_str(node_id_str);
                let node_type = parse_node_type(node_type_str);

                // 从 payload 提取 subscriptions
                let subscriptions: Vec<String> = payload["subscriptions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // 注册到 Registry 和 Broker
                self.registry.register(
                    node_id.clone(),
                    node_type,
                    subscriptions.clone(),
                );
                self.broker.register_identity(node_id.clone(), identity.to_vec());
                for pattern in subscriptions {
                    self.broker.subscribe(node_id.clone(), pattern);
                }
                tracing::info!("Node 注册成功：{} ({:?})", node_id, node_type);
            }

            // 系统消息：Node 心跳
            "sys/heartbeat" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id = NodeId::from_str(node_id_str);
                self.registry.heartbeat(&node_id);
                tracing::trace!("心跳：{}", node_id);
            }

            // 系统消息：Node 注销
            "sys/deregister" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id = NodeId::from_str(node_id_str);
                self.deregister_node(&node_id);
            }

            // Agent spawn 请求
            t if t.ends_with("/spawn") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let agent_type_str = payload["agent_type"].as_str().unwrap_or("general-purpose");
                let model = payload["model"].as_str().map(String::from);
                let task_desc = payload["task_description"].as_str().unwrap_or("");

                let agent_type = crate::agent::AgentType::from_str(agent_type_str);
                let parent_id = NodeId::from_str(
                    incoming.message.header.src_node.as_str()
                );

                match self.request_spawn_subagent(transport, &parent_id, agent_type, model, task_desc.to_string()).await {
                    Ok(new_id) => {
                        tracing::info!("子 Agent 已 spawn：{}", new_id);
                    }
                    Err(e) => {
                        tracing::warn!("子 Agent spawn 失败：{}", e);
                    }
                }
            }

            // 普通业务消息：路由到订阅者
            _ => {
                self.route_and_forward(&incoming.message, transport).await?;
            }
        }

        Ok(())
    }

    /// 将消息路由到所有订阅者并通过 ROUTER 发送
    async fn route_and_forward(
        &self,
        msg: &Message,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        let targets = self.broker.route_message(msg)?;

        if targets.is_empty() {
            return Ok(());
        }

        // broker.route_message 已经编码了消息帧，直接使用返回的 frames
        // targets 格式：(identity, encoded_frames)
        for (identity, frames) in targets {
            let identity_bytes = Bytes::from(identity);
            let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
            transport.send_to(identity_bytes, frames_bytes).await?;
        }

        Ok(())
    }

    /// 注册新 Node（由外部调用，如 Launcher）
    pub fn register_node(
        &mut self,
        id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    ) {
        self.registry.register(id.clone(), node_type, subscriptions.clone());
        self.broker.register_identity(id.clone(), identity);
        for pattern in subscriptions {
            self.broker.subscribe(id.clone(), pattern);
        }
        tracing::info!("Node 注册：{} ({:?})", id, node_type);
    }

    /// 注销 Node
    pub fn deregister_node(&mut self, id: &NodeId) {
        let node_info = self.registry.get(id);
        self.registry.deregister(id);
        self.broker.deregister_identity(id);
        if let Some(info) = node_info {
            tracing::info!("Node 注销：{} ({:?})", id, info.node_type);
        }
    }

    /// 处理 Node 心跳
    pub fn handle_heartbeat(&mut self, id: &NodeId) {
        self.registry.heartbeat(id);
    }

    /// 请求 spawn 子 Agent
    async fn request_spawn_subagent(
        &mut self,
        transport: &mut KernelTransport,
        parent_id: &NodeId,
        agent_type: crate::agent::AgentType,
        model: Option<String>,
        task_description: String,
    ) -> Result<NodeId> {
        // 检查子 Agent 数量限制
        let current_subagents = self.registry.find_by_type(NodeType::Agent).len();
        if current_subagents >= self.config.max_subagents {
            return Err(anyhow::anyhow!(
                "子 Agent 数量已达上限 {}",
                self.config.max_subagents
            ));
        }

        let new_id = NodeId::new();
        tracing::info!(
            "spawn 子 Agent：{} (parent={}, type={:?}, model={:?})",
            new_id, parent_id, agent_type, model
        );

        // 注册子 Agent
        let subscriptions = vec![
            format!("agent/{}/input", new_id),
            format!("agent/{}/tool_result", new_id),
            "sampler/*/stream".into(),
        ];
        self.registry.register(new_id.clone(), NodeType::Agent, subscriptions);

        // 广播 spawn 事件
        let spawn_msg = FrameCodec::new_message(
            Topic::sys_spawn(),
            "kernel",
            &serde_json::json!({
                "node_id": new_id.to_string(),
                "node_type": "agent",
                "agent_type": format!("{:?}", agent_type),
                "parent_id": parent_id.to_string(),
                "model": model,
                "task_description": task_description,
            }),
        )?;

        let frames: Vec<Bytes> = FrameCodec::encode(&spawn_msg)?
            .into_iter()
            .map(Bytes::from)
            .collect();
        transport.broadcast(frames).await?;

        Ok(new_id)
    }

    /// 广播 Node 下线事件
    async fn broadcast_node_deregister(&self, transport: &mut KernelTransport, node_id: &NodeId) {
        tracing::info!("广播 Node 下线：{}", node_id);
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_deregister(),
            "kernel",
            &serde_json::json!({ "node_id": node_id.to_string() }),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                let _ = transport.broadcast(bytes_frames).await;
            }
        }
    }

    /// 广播全局关闭信号
    async fn broadcast_shutdown(&self, transport: &mut KernelTransport) {
        tracing::info!("广播全局关闭信号");
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_shutdown(),
            "kernel",
            &serde_json::json!({}),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                let _ = transport.broadcast(bytes_frames).await;
            }
        }
    }

    /// 停止 Kernel
    pub async fn stop(&mut self) {
        self.running = false;
        tracing::info!("Kernel 关闭");
    }

    /// 获取当前在线 Node 数量
    pub fn node_count(&self) -> usize {
        self.registry.len()
    }
}

/// 解析 NodeType 字符串
fn parse_node_type(s: &str) -> NodeType {
    match s {
        "kernel" => NodeType::Kernel,
        "agent" => NodeType::Agent,
        "tool" => NodeType::Tool,
        "sampler" => NodeType::Sampler,
        "state" => NodeType::State,
        "tui" => NodeType::TUI,
        "plugin" => NodeType::Plugin,
        _ => NodeType::Plugin,
    }
}
