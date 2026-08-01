//! MCP Server 模块 - 外部工具接入 ccode 的标准接口
//!
//! MCP（Model Context Protocol）Server 允许外部 AI Agent 通过
//! JSON-RPC 2.0 协议调用 ccode 的内置工具（read/write/edit/bash/glob/grep）。
//!
//! ## 架构
//!
//! MCP Server 作为独立 tokio task 运行，不阻塞 Kernel 主事件循环：
//! - 通过消息总线与 ToolNode 通信
//! - 支持两种传输方式：Stdio（标准输入输出）和 SSE（HTTP Server-Sent Events）
//!
//! ## 消息流
//!
//! ```text
//! 外部 Agent → MCP Server → 消息总线 → ToolNode → 执行结果 → 消息总线 → MCP Server → 外部 Agent
//! ```

pub mod transport;
pub mod tool_registry;
pub mod handler;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::message::Message;

/// MCP 传输方式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum McpTransportKind {
    /// 标准输入输出传输（适合进程间通信）
    Stdio,
    /// HTTP SSE 传输（适合远程访问）
    Sse {
        /// SSE 服务监听端口
        port: u16,
    },
}

/// MCP Server 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// 传输方式
    pub transport: McpTransportKind,
    /// 服务名称
    pub name: String,
    /// 服务版本
    pub version: String,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            transport: McpTransportKind::Stdio,
            name: "ccode-mcp".into(),
            version: "0.1.0".into(),
        }
    }
}

/// MCP Server 运行句柄
///
/// 持有 MCP Server 的 tokio task 句柄和关闭通道，
/// 可用于等待任务完成或主动关闭。
pub struct McpServerHandle {
    /// MCP Server 的 tokio task 句柄
    task_handle: JoinHandle<()>,
    /// 关闭信号发送端
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl McpServerHandle {
    /// 请求关闭 MCP Server
    pub fn shutdown(self) {
        if self.shutdown_tx.send(()).is_err() {
            tracing::warn!("MCP Server 关闭信号发送失败，可能已停止");
        }
    }

    /// 等待 MCP Server task 结束
    pub async fn join(self) {
        if let Err(e) = self.task_handle.await {
            tracing::warn!("MCP Server task 异常退出：{}", e);
        }
    }
}

/// MCP Server 主结构
///
/// 持有配置、工具注册表和消息总线发送端，
/// 在独立 tokio task 中运行，接收并处理外部 JSON-RPC 请求。
pub struct McpServer {
    /// 服务配置
    config: McpServerConfig,
    /// 工具注册表
    tool_registry: tool_registry::McpToolRegistry,
    /// 消息总线发送端（用于向 ToolNode 发送工具调用请求）
    #[allow(dead_code)]
    bus_tx: tokio::sync::mpsc::Sender<Message>,
}

impl McpServer {
    /// 创建 MCP Server 实例
    pub fn new(config: McpServerConfig, bus_tx: tokio::sync::mpsc::Sender<Message>) -> Self {
        let tool_registry = tool_registry::McpToolRegistry::new(bus_tx.clone());
        Self {
            config,
            tool_registry,
            bus_tx,
        }
    }

    /// 启动 MCP Server（在独立 tokio task 中运行）
    ///
    /// 根据 transport 配置选择传输方式，进入消息处理循环。
    /// 返回句柄可用于关闭或等待。
    pub fn run(self) -> McpServerHandle {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let transport_kind = self.config.transport.clone();
        let server_name = self.config.name.clone();
        let server_version = self.config.version.clone();
        let registry = self.tool_registry;

        let task_handle = tokio::spawn(async move {
            match transport_kind {
                McpTransportKind::Stdio => {
                    let mut transport_impl = transport::StdioTransport::new();
                    Self::event_loop(
                        &mut transport_impl,
                        registry,
                        &server_name,
                        &server_version,
                        shutdown_rx,
                    ).await;
                }
                McpTransportKind::Sse { port } => {
                    let mut transport_impl = transport::SseTransport::new(port);
                    Self::event_loop(
                        &mut transport_impl,
                        registry,
                        &server_name,
                        &server_version,
                        shutdown_rx,
                    ).await;
                }
            }
        });

        McpServerHandle {
            task_handle,
            shutdown_tx,
        }
    }

    /// 主事件循环 — 从传输层接收消息，处理后发送响应
    async fn event_loop<T: transport::McpTransport>(
        transport: &mut T,
        registry: tool_registry::McpToolRegistry,
        server_name: &str,
        server_version: &str,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        tracing::info!("MCP Server 事件循环已启动");

        loop {
            tokio::select! {
                // 接收客户端请求
                result = transport.recv_message() => {
                    match result {
                        Ok(request_str) => {
                            let response = handler::handle_request(
                                &request_str,
                                &registry,
                                server_name,
                                server_version,
                            );
                            match response {
                                Ok(Some(response_str)) => {
                                    if let Err(e) = transport.send_message(&response_str).await {
                                        tracing::warn!("MCP Server 发送响应失败：{}", e);
                                    }
                                }
                                Ok(None) => {
                                    // 通知类消息，无需响应
                                }
                                Err(e) => {
                                    tracing::warn!("MCP Server 处理请求失败：{}", e);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("MCP Server 接收消息失败：{}", e);
                        }
                    }
                }
                // 关闭信号
                _ = &mut shutdown_rx => {
                    tracing::info!("MCP Server 收到关闭信号，退出事件循环");
                    break;
                }
            }
        }

        tracing::info!("MCP Server 事件循环已退出");
    }
}
