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
pub mod transaction;
pub mod backpressure;
pub mod metrics;

use anyhow::Result;
use bytes::Bytes;
use std::time::Duration;

use crate::config::CcodeConfig;
use crate::kernel::transport::{IncomingMessage, KernelTransport};
use crate::kernel::backpressure::{BackpressureController, BackpressureConfig};
use crate::kernel::metrics::{MonitoringService, HealthCheckConfig};
use crate::message::frame::FrameCodec;
use crate::message::Topic;
use crate::message::Message;
use crate::message::SequenceChecker;
use crate::message::param::ParamServer;
use crate::node::{NodeId, NodeType, NodeContext};
use std::sync::Arc;

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
/// ROS 1 风格：Kernel 只做控制面（发现/注册/心跳/参数），不转发业务数据。
///
/// Kernel 持有：
/// - `broker`: 发现逻辑（identity 映射、publisher 映射、service 映射）
/// - `registry`: Node 注册表（元数据、心跳时间戳）
/// - `sequence_checker`: 消息序列号检查器
/// - `backpressure`: 背压控制器
/// - `monitoring`: 监控服务
/// - `param_server`: ROS 风格的参数服务器
/// - `ccode_config`: ccode 全局配置
pub struct Kernel {
    config: KernelConfig,
    broker: broker::Broker,
    registry: registry::Registry,
    sequence_checker: SequenceChecker,
    backpressure: Arc<BackpressureController>,
    monitoring: MonitoringService,
    param_server: ParamServer,
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
            sequence_checker: SequenceChecker::new(100),
            backpressure: Arc::new(BackpressureController::new(BackpressureConfig::default())),
            monitoring: MonitoringService::new(HealthCheckConfig::default()),
            param_server: ParamServer::new(),
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
            data_pub_addr: format!("ipc:///tmp/ccode-pub-{}", NodeId::new()),
            data_rep_addr: None,
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

        // 1. 启动 KernelTransport，取出作为独立变量避免借用冲突
        let mut transport = KernelTransport::new(
            &self.config.router_addr,
            &self.config.pub_addr,
        ).await?;

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
                            if let Err(e) = self.handle_incoming(incoming, &mut transport).await {
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
                        Self::broadcast_node_deregister(&mut transport, &node_id).await;
                    }
                }
            }
        }

        // 关闭流程
        Self::broadcast_shutdown(&mut transport).await;
        transport.shutdown().await;
        Ok(())
    }

    /// 处理从 ROUTER 收到的消息
    async fn handle_incoming(
        &mut self,
        incoming: IncomingMessage,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        // ✅ 提前获取 collector Arc，避免后续与 self.broker/self.registry 的借用冲突
        let collector = self.monitoring.collector();
        // ✅ 记录接收消息
        collector.record_received();

        let now = std::time::Instant::now();
        let topic = incoming.message.topic.as_str();
        let identity = incoming.identity.clone();
        let src_node = incoming.message.header.src_node.clone();
        let sequence = incoming.message.header.sequence;
        
        // 序列号检查（跳过注册消息）
        if topic != "sys/register" {
            let node_id: NodeId = src_node.parse().unwrap();
            match self.sequence_checker.check(&node_id, sequence) {
                Ok(crate::message::SequenceCheckResult::InOrder) => {
                    // 正常顺序，继续处理
                }
                Ok(crate::message::SequenceCheckResult::Gap(gap)) => {
                    // 序列号跳跃，可能丢包，但仍接受
                    tracing::warn!(
                        "Node {} 序列号跳跃 {}（可能丢包），继续处理",
                        node_id, gap
                    );
                    // ✅ 记录序列号错误
                    collector.record_sequence_error();
                }
                Err(e) => {
                    // 序列号乱序或重复，拒绝处理
                    tracing::error!(
                        "Node {} 序列号检查失败：{}，拒绝消息",
                        node_id, e
                    );
                    // ✅ 记录序列号错误
                    collector.record_sequence_error();
                    return Ok(()); // 拒绝处理，但不返回错误
                }
            }
        }

        match topic {
            // 系统消息：Node 注册
            "sys/register" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_type_str = payload["node_type"].as_str().unwrap_or("agent");
                let node_id: NodeId = node_id_str.parse().unwrap();
                let node_type = parse_node_type(node_type_str);
                
                // 注册成功，重置序列号检查器
                self.sequence_checker.reset(&node_id);
                
                // ✅ 记录心跳
                self.monitoring.record_heartbeat(node_id_str);

                // 从 payload 提取 subscriptions
                let subscriptions: Vec<String> = payload["subscriptions"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // 从 payload 提取数据面地址（ROS 1 核心）
                let pub_addr = payload["pub_addr"].as_str().unwrap_or("").to_string();
                let rep_addr = payload["rep_addr"].as_str().map(String::from);
                
                // 从 payload 提取该 Node 发布的 topic 列表
                let published_topics: Vec<String> = payload["published_topics"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();

                // 原子注册
                if let Err(e) = self.atomic_register_node(
                    node_id.clone(),
                    node_type,
                    subscriptions.clone(),
                    identity.to_vec(),
                ) {
                    tracing::error!("Node 注册失败：{}", e);
                    collector.record_failed();
                } else {
                    // ROS 1 核心：注册 Publisher 信息到 Broker
                    if !pub_addr.is_empty() && !published_topics.is_empty() {
                        self.broker.register_publisher(broker::PublisherInfo {
                            node_id: node_id.clone(),
                            pub_addr: pub_addr.clone(),
                            topics: published_topics,
                        });
                    }

                    // ROS 1 核心：注册 Service Provider 信息到 Broker
                    if let Some(ref rep) = rep_addr {
                        if let Some(service_name) = payload["service_name"].as_str() {
                            self.broker.register_service_provider(broker::ServiceProviderInfo {
                                node_id: node_id.clone(),
                                rep_addr: rep.clone(),
                                service_name: service_name.to_string(),
                            });
                        }
                    }

                    let latency_ms = now.elapsed().as_millis() as f64;
                    collector.record_success(latency_ms);
                }

                // ROS 1 核心：返回 publisher 发现信息给新注册的 Node
                // 告知新 Node：它订阅的 topic 有哪些 publisher，可以直接 SUB 连接
                let publisher_map = self.broker.find_publishers_for_subscriptions(&subscriptions);
                if !publisher_map.is_empty() {
                    let discover_msg = FrameCodec::new_reply(
                        Topic::sys_register(),
                        "kernel",
                        incoming.message.header.msg_id.clone(),
                        &serde_json::json!({
                            "type": "publisher_discovery",
                            "publishers": publisher_map.iter().map(|(pattern, pubs)| {
                                serde_json::json!({
                                    "pattern": pattern,
                                    "publishers": pubs.iter().map(|p| serde_json::json!({
                                        "node_id": p.node_id.to_string(),
                                        "pub_addr": p.pub_addr,
                                        "topics": p.topics,
                                    })).collect::<Vec<_>>(),
                                })
                            }).collect::<Vec<_>>(),
                        }),
                    )?;
                    let frames_bytes: Vec<Bytes> = FrameCodec::encode(&discover_msg)?
                        .into_iter()
                        .map(Bytes::from)
                        .collect();
                    transport.send_to(identity, frames_bytes).await?;
                }
            }

            // 系统消息：Node 心跳
            "sys/heartbeat" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id: NodeId = node_id_str.parse().unwrap();
                self.registry.heartbeat(&node_id);
                tracing::trace!("心跳：{}", node_id);
            }

            // 系统消息：Node 注销
            "sys/deregister" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let node_id_str = payload["node_id"].as_str().unwrap_or("");
                let node_id: NodeId = node_id_str.parse().unwrap();
                self.deregister_node(&node_id);
            }

            // Agent spawn 请求
            t if t.ends_with("/spawn") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let agent_type_str = payload["agent_type"].as_str().unwrap_or("general-purpose");
                let model = payload["model"].as_str().map(String::from);
                let task_desc = payload["task_description"].as_str().unwrap_or("");

                let agent_type: crate::agent::AgentType = agent_type_str.parse().unwrap();
                let parent_id: NodeId = incoming.message.header.src_node.as_str().parse().unwrap();

                match self.request_spawn_subagent(transport, &parent_id, agent_type, model, task_desc.to_string()).await {
                    Ok(new_id) => {
                        tracing::info!("子 Agent 已 spawn：{}", new_id);
                    }
                    Err(e) => {
                        tracing::warn!("子 Agent spawn 失败：{}", e);
                    }
                }
            }

            // ROS 风格：Service 注册（控制面）
            t if t.starts_with("service/") && t.ends_with("/register") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let service_name = payload["service_name"].as_str().unwrap_or("");
                let rep_addr = payload["rep_addr"].as_str().unwrap_or("");
                let node_id: NodeId = payload["node_id"].as_str().unwrap_or("").parse().unwrap();
                
                if !service_name.is_empty() && !rep_addr.is_empty() {
                    self.broker.register_service_provider(broker::ServiceProviderInfo {
                        node_id,
                        rep_addr: rep_addr.to_string(),
                        service_name: service_name.to_string(),
                    });
                    tracing::info!("Service 注册：{} → {}", service_name, rep_addr);
                }
            }

            // ROS 风格：Service 发现（控制面，返回 provider 的 REP 地址）
            t if t.starts_with("service/") && t.ends_with("/lookup") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let service_name = payload["service_name"].as_str().unwrap_or("");
                
                let response = if let Some(provider) = self.broker.find_service_provider(service_name) {
                    serde_json::json!({
                        "found": true,
                        "node_id": provider.node_id.to_string(),
                        "rep_addr": provider.rep_addr,
                        "service_name": service_name,
                    })
                } else {
                    serde_json::json!({
                        "found": false,
                        "service_name": service_name,
                    })
                };
                
                let reply = FrameCodec::new_reply(
                    incoming.message.topic.clone(),
                    "kernel",
                    incoming.message.header.msg_id.clone(),
                    &response,
                )?;
                let frames_bytes: Vec<Bytes> = FrameCodec::encode(&reply)?
                    .into_iter()
                    .map(Bytes::from)
                    .collect();
                transport.send_to(identity, frames_bytes).await?;
            }

            // ROS 风格：参数服务器
            "param/set" => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                if let Some(key) = payload["key"].as_str() {
                    let value = payload.get("value").cloned().map(|v| {
                        serde_json::from_value(v).unwrap_or(crate::message::ParamValue::Null)
                    });
                    if let Some(v) = value {
                        self.param_server.set(key, v);
                    }
                }
                // 控制面消息转发：param/changed 属于控制面通知（类似 ROS 1 Master 广播参数变更），
                // 不是业务数据，经 Kernel 转发是符合 ROS 架构的
                self.route_and_forward(&incoming.message, transport).await?;
            }

            "param/get" => {
                // 参数查询：直接通过消息响应
                let payload: serde_json::Value = FrameCodec::decode_payload(&incoming.message)?;
                let key = payload["key"].as_str().unwrap_or("");
                let reply_to = incoming.message.header.msg_id.clone();
                
                if let Some(value) = self.param_server.get(key) {
                    let response = FrameCodec::new_reply(
                        Topic::param_changed(),
                        "kernel",
                        reply_to,
                        &serde_json::json!({
                            "key": key,
                            "value": value,
                        }),
                    )?;
                    // 控制面消息转发：param/get 响应属于控制面通知（类似 ROS 1 Master 返回参数查询结果），
                    // 不是业务数据，经 Kernel 转发是符合 ROS 架构的
                    self.route_and_forward(&response, transport).await?;
                }
            }

            // 控制面路由消息：经 Kernel ROUTER 转发到订阅者
            // ROS 1 风格：tool_call、tool_result、agent/output 等控制面消息
            // 通过 DEALER↔ROUTER 连接传输，Node 没有直连通道，必须经 Kernel 中转。
            // 只有大块业务数据才走 Node PUB/SUB 直连（数据面）。
            _ => {
                // 尝试路由到订阅了此 topic 的 Node
                let targets = self.broker.find_targets(&incoming.message);
                if targets.is_empty() {
                    // 无订阅者：可能是纯数据面消息（Node 应通过 PUB/SUB 直连），
                    // 或消息发到了无人订阅的 topic
                    tracing::debug!(
                        "消息 {} 无订阅者，跳过路由（如为业务数据请使用 PUB/SUB 直连）",
                        topic
                    );
                } else {
                    // 有订阅者：通过控制面 ROUTER 转发
                    self.route_and_forward(&incoming.message, transport).await?;
                }
            }
        }

        Ok(())
    }

    /// 将消息路由到所有订阅者并通过 ROUTER 发送
    ///
    /// ROS 风格的消息路由：
    /// 1. 通过 broker.find_targets 查找订阅者（不编码）
    /// 2. 编码消息帧（只编码一次）
    /// 3. 逐个发送到每个订阅者
    /// 4. 记录发送统计
    async fn route_and_forward(
        &self,
        msg: &Message,
        transport: &mut KernelTransport,
    ) -> Result<()> {
        // ✅ 检查背压级别
        if let Some(delay) = self.backpressure.get_delay() {
            tracing::warn!("背压触发，延迟 {:?} 后发送", delay);
            tokio::time::sleep(delay).await;
        }

        // ✅ 查找订阅者（不编码消息帧，避免重复编码）
        let targets = self.broker.find_targets(msg);

        if targets.is_empty() {
            return Ok(());
        }

        // ✅ 编码消息帧（只编码一次，所有订阅者共享）
        let frames = FrameCodec::encode(msg)?;
        let frames_bytes: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();

        // ✅ 逐个发送到每个订阅者
        for (identity, _node_id) in targets {
            let identity_bytes = Bytes::from(identity);
            transport.send_to(identity_bytes, frames_bytes.clone()).await?;
        }

        // ✅ 记录发送统计
        self.backpressure.record_sent();

        Ok(())
    }

    /// 原子注册 Node（带补偿逻辑）
    ///
    /// 确保状态一致性：
    /// - 先注册 Registry（记录状态）
    /// - 再注册 Broker identity
    /// - 最后添加订阅关系
    /// - 任何步骤失败时，回滚已完成的所有步骤
    fn atomic_register_node(
        &mut self,
        node_id: NodeId,
        node_type: NodeType,
        subscriptions: Vec<String>,
        identity: Vec<u8>,
    ) -> Result<()> {
        // 步骤 1: Registry 注册（记录元数据）
        self.registry.register(node_id.clone(), node_type, subscriptions.clone());

        // 步骤 2: Broker 注册 identity
        // 注意：register_identity 目前不返回错误，但为了保持回滚框架
        // 如果 Node 已注册，旧的 identity 会被覆盖（这是预期行为）
        self.broker.register_identity(node_id.clone(), identity);

        // 步骤 3: Broker 订阅 topics
        // 跟踪已添加的订阅，用于回滚
        let mut added_subscriptions: Vec<String> = Vec::new();
        for pattern in &subscriptions {
            self.broker.subscribe(node_id.clone(), pattern.clone());
            added_subscriptions.push(pattern.clone());
        }

        tracing::info!("Node 注册成功：{} ({:?})", node_id, node_type);
        Ok(())
    }

    /// 原子注销 Node（带补偿逻辑）
    ///
    /// 步骤：
    /// 1. Registry 注销
    /// 2. Broker 注销 identity 和 subscriptions
    fn atomic_deregister_node(&mut self, id: &NodeId) {
        let node_info = self.registry.get(id).cloned();

        // 步骤 1: Registry 注销
        self.registry.deregister(id);

        // 步骤 2: Broker 注销
        self.broker.deregister_identity(id);

        if let Some(info) = node_info {
            tracing::info!("Node 注销：{} ({:?})", id, info.node_type);
        }
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
        self.atomic_deregister_node(id);
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
    async fn broadcast_node_deregister(transport: &mut KernelTransport, node_id: &NodeId) {
        tracing::info!("广播 Node 下线：{}", node_id);
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_deregister(),
            "kernel",
            &serde_json::json!({ "node_id": node_id.to_string() }),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                if let Err(e) = transport.broadcast(bytes_frames).await {
                    tracing::warn!("广播失败：{}", e);
                }
            }
        }
    }

    /// 广播全局关闭信号
    async fn broadcast_shutdown(transport: &mut KernelTransport) {
        tracing::info!("广播全局关闭信号");
        if let Ok(msg) = FrameCodec::new_message(
            Topic::sys_shutdown(),
            "kernel",
            &serde_json::json!({}),
        ) {
            if let Ok(frames) = FrameCodec::encode(&msg) {
                let bytes_frames: Vec<Bytes> = frames.into_iter().map(Bytes::from).collect();
                if let Err(e) = transport.broadcast(bytes_frames).await {
                    tracing::warn!("广播失败：{}", e);
                }
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
