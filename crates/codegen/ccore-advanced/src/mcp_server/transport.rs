//! MCP 传输层实现 — Stdio 和 SSE 两种传输方式
//!
//! - StdioTransport：从 stdin 读取 JSON-RPC 消息，写入 stdout
//! - SseTransport：HTTP 服务器，POST 接收请求，GET 返回 SSE 流

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// MCP 传输层 trait — 所有传输方式必须实现
#[async_trait]
pub trait McpTransport: Send {
    /// 接收一条 JSON-RPC 消息（阻塞直到收到完整消息）
    fn recv_message(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>>;

    /// 发送一条 JSON-RPC 消息
    async fn send_message(&mut self, msg: &str) -> Result<()>;
}

/// 标准输入输出传输
///
/// 从 stdin 逐行读取 JSON-RPC 请求，将响应写入 stdout。
/// 适合进程间通信（如 Claude Desktop 通过 stdin/stdout 与 MCP Server 交互）。
pub struct StdioTransport {
    /// 缓冲读取器（stdin）
    stdin_reader: BufReader<tokio::io::Stdin>,
    /// 标准输出
    stdout: tokio::io::Stdout,
}

impl StdioTransport {
    /// 创建 Stdio 传输实例
    pub fn new() -> Self {
        Self {
            stdin_reader: BufReader::new(tokio::io::stdin()),
            stdout: tokio::io::stdout(),
        }
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    fn recv_message(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async {
            let mut line = String::new();
            match self.stdin_reader.read_line(&mut line).await {
                Ok(0) => Err(anyhow!("stdin 已关闭")),
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        // 空行，继续读取
                        return self.recv_message().await;
                    }
                    Ok(trimmed.to_string())
                }
                Err(e) => Err(anyhow!("stdin 读取失败：{}", e)),
            }
        })
    }

    async fn send_message(&mut self, msg: &str) -> Result<()> {
        // JSON-RPC over Stdio：每条消息后追加换行符
        let output = format!("{}\n", msg);
        self.stdout.write_all(output.as_bytes()).await
            .map_err(|e| anyhow!("stdout 写入失败：{}", e))?;
        self.stdout.flush().await
            .map_err(|e| anyhow!("stdout flush 失败：{}", e))?;
        Ok(())
    }
}

/// SSE（Server-Sent Events）传输
///
/// HTTP 服务器模式：
/// - POST /message：接收客户端请求
/// - GET /sse：返回 SSE 事件流，推送响应
///
/// 适合远程 Agent 通过 HTTP 协议与 MCP Server 交互。
pub struct SseTransport {
    /// 监听端口
    #[allow(dead_code)]
    port: u16,
    /// 请求接收通道
    request_rx: tokio::sync::mpsc::Receiver<String>,
    /// 请求发送通道（传给 axum handler）
    #[allow(dead_code)]
    request_tx: tokio::sync::mpsc::Sender<String>,
    /// 响应发送通道
    response_tx: tokio::sync::broadcast::Sender<String>,
}

impl SseTransport {
    /// 创建 SSE 传输实例
    ///
    /// 立即启动 HTTP 服务器。
    pub fn new(port: u16) -> Self {
        let (request_tx, request_rx) = tokio::sync::mpsc::channel::<String>(64);
        let (response_tx, _) = tokio::sync::broadcast::channel::<String>(64);

        let server_addr = format!("0.0.0.0:{}", port);
        let req_tx = request_tx.clone();
        let resp_tx = response_tx.clone();

        // 在独立 tokio task 中启动 HTTP 服务器
        tokio::spawn(async move {
            let app = Self::build_router(req_tx, resp_tx);
            let listener = match tokio::net::TcpListener::bind(&server_addr).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("MCP SSE 服务器绑定 {} 失败：{}", server_addr, e);
                    return;
                }
            };
            tracing::info!("MCP SSE 服务器已启动，监听 {}", server_addr);
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!("MCP SSE 服务器异常退出：{}", e);
            }
        });

        Self {
            port,
            request_rx,
            request_tx,
            response_tx,
        }
    }

    /// 构建 axum 路由
    fn build_router(
        request_tx: tokio::sync::mpsc::Sender<String>,
        response_tx: tokio::sync::broadcast::Sender<String>,
    ) -> axum::Router {
        use axum::routing::{get, post};

        let state = std::sync::Arc::new(SseState {
            request_tx,
            response_tx,
        });

        axum::Router::new()
            .route("/message", post(handle_post_message))
            .route("/sse", get(handle_sse_stream))
            .with_state(state)
    }
}

/// SSE 共享状态
#[derive(Clone)]
struct SseState {
    /// 请求发送通道
    request_tx: tokio::sync::mpsc::Sender<String>,
    /// 响应广播通道
    response_tx: tokio::sync::broadcast::Sender<String>,
}

/// POST /message 处理器 — 接收客户端请求
async fn handle_post_message(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<SseState>>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let request_str = match serde_json::to_string(&body) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("MCP SSE 请求序列化失败：{}", e);
            return axum::Json(serde_json::json!({
                "error": format!("序列化失败：{}", e)
            }));
        }
    };

    if let Err(e) = state.request_tx.send(request_str).await {
        tracing::warn!("MCP SSE 请求通道发送失败：{}", e);
        return axum::Json(serde_json::json!({
            "error": format!("内部通道错误：{}", e)
        }));
    }

    // 返回简单的接收确认
    axum::Json(serde_json::json!({"status": "received"}))
}

/// GET /sse 处理器 — 返回 SSE 事件流
async fn handle_sse_stream(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<SseState>>,
) -> axum::response::Sse<impl futures::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>> {
    use axum::response::sse::{Event, KeepAlive};

    let mut rx = state.response_tx.subscribe();

    // 创建 SSE 事件流
    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    yield Ok(Event::default().data(msg));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("MCP SSE 流滞后，跳过 {} 条消息", n);
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    axum::response::Sse::new(stream)
        .keep_alive(KeepAlive::default())
}

#[async_trait]
impl McpTransport for SseTransport {
    fn recv_message(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + '_>> {
        Box::pin(async {
            self.request_rx
                .recv()
                .await
                .ok_or_else(|| anyhow!("MCP SSE 请求通道已关闭"))
        })
    }

    async fn send_message(&mut self, msg: &str) -> Result<()> {
        self.response_tx
            .send(msg.to_string())
            .map_err(|e| anyhow!("MCP SSE 响应广播失败：{}", e))?;
        Ok(())
    }
}
