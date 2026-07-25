//! Node trait 定义 - 所有 Node 进程的统一接口

pub mod agent;
pub mod sampler;
pub mod tui;
pub mod tool;
pub mod state;
pub mod transport;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::message::Message;
use crate::node::transport::NodeTransportHandle;

/// Node 唯一标识
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    pub fn from_str(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Node 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeType {
    Kernel,
    Agent,
    Tool,
    Sampler,
    State,
    TUI,
    Plugin,
}

impl NodeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Kernel => "kernel",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::Sampler => "sampler",
            Self::State => "state",
            Self::TUI => "tui",
            Self::Plugin => "plugin",
        }
    }
}

/// Node 启动配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node 唯一 ID
    pub id: NodeId,
    /// Node 类型
    pub node_type: NodeType,
    /// 使用的工作目录
    pub working_dir: String,
    /// 模型名称（Agent/Sampler 专用）
    pub model: Option<String>,
    /// 权限模式
    pub permission_mode: PermissionMode,
    /// 额外参数
    pub extra: serde_json::Value,
}

/// 权限模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// 自动允许所有操作
    Yolo,
    /// 信任已验证的操作，询问未知操作
    Trust,
    /// 每次操作都需要确认
    Ask,
}

/// Node 运行上下文，提供消息总线连接信息
#[derive(Debug, Clone)]
pub struct NodeContext {
    /// Kernel 的 ROUTER socket 地址
    pub router_addr: String,
    /// Kernel 的 PUB socket 地址
    pub pub_addr: String,
}

/// Node trait - 所有 Node 进程必须实现
///
/// 每个 Node 通过消息总线收发消息，业务逻辑在 handle_message 中实现。
/// 传输层由 run_node() 统一管理，Node 只需关注消息处理。
#[async_trait]
pub trait Node: Send + Sync {
    /// 获取 Node 类型
    fn node_type(&self) -> NodeType;

    /// 获取 Node ID
    fn node_id(&self) -> &NodeId;

    /// 启动 Node（初始化内部状态）
    ///
    /// 传输层已由 run_node() 创建并连接，Node 在此方法中初始化业务状态。
    async fn start(&mut self, ctx: NodeContext) -> anyhow::Result<()>;

    /// 处理收到的消息
    ///
    /// Node 在此方法中实现业务逻辑。如果需要发送消息，
    /// 通过 transport 参数提供的句柄发送。
    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()>;

    /// 获取此 Node 订阅的 topic 模式列表
    fn subscriptions(&self) -> Vec<String>;

    /// 停止 Node
    async fn stop(&mut self) -> anyhow::Result<()>;
}
