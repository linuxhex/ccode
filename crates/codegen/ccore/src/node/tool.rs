//! Tool Node - 接收工具调用请求，执行并返回结果
//!
//! Tool Node 的完整工作流：
//! 1. 订阅 agent/*/tool_call topic
//! 2. 收到 ToolCallRequest → 权限检查 → 并发限流 → 执行工具 → 返回 ToolCallResult
//! 3. 结果通过消息总线发送到 agent/{src}/tool_result
//!
//! 并发控制：内置 Semaphore（默认 20 并发），防止工具执行过载。
//! 对应 ANS 的 tool_semaphore，但运行在 ToolNode 本地（独立进程无法共享 ANS）。

use async_trait::async_trait;
use tokio::sync::Semaphore;

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext, PermissionMode};
use crate::node::transport::NodeTransportHandle;
use crate::tools::bridge::ToolBridge;
use crate::tools::ToolCallRequest;

/// 默认最大并发工具执行数
const DEFAULT_MAX_CONCURRENT_TOOLS: usize = 20;

/// Tool Node 实现
pub struct ToolNode {
    id: NodeId,
    /// 工具桥接器
    bridge: ToolBridge,
    /// 全局权限模式
    permission_mode: PermissionMode,
    /// 并发限流信号量（对应 ANS 的 tool_semaphore）
    concurrency_semaphore: Semaphore,
}

impl ToolNode {
    pub fn new(id: NodeId) -> Self {
        Self {
            id,
            bridge: ToolBridge::new(),
            permission_mode: PermissionMode::Trust,
            concurrency_semaphore: Semaphore::new(DEFAULT_MAX_CONCURRENT_TOOLS),
        }
    }

    /// 设置权限模式
    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        self.permission_mode = mode;
    }

    /// 处理工具调用请求，通过消息总线返回结果
    async fn handle_tool_call(
        &self,
        request: ToolCallRequest,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        // 并发限流：获取信号量许可
        let _permit = match self.concurrency_semaphore.try_acquire() {
            Ok(permit) => permit,
            Err(_) => {
                tracing::warn!(
                    available = self.concurrency_semaphore.available_permits(),
                    "工具并发已满，等待可用许可"
                );
                // 等待可用许可
                self.concurrency_semaphore.acquire().await
                    .map_err(|_| anyhow::anyhow!("信号量已关闭"))?
            }
        };

        // 权限检查
        if self.bridge.needs_confirmation(&request.tool_name, self.permission_mode) {
            match self.permission_mode {
                PermissionMode::Ask => {
                    // Ask 模式下拒绝未确认的工具调用
                    AgentMetrics::global().record_error("tool_permission_denied");
                    let result_msg = FrameCodec::new_message(
                        Topic::agent_tool_result(&request.agent_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "tool_call_id": request.tool_call_id,
                            "output": format!("工具 {} 需要用户确认，当前权限模式为 Ask，已拒绝执行", request.tool_name),
                            "success": false,
                            "duration_ms": 0,
                        }),
                    )?;
                    transport.send_message(&result_msg).await?;
                    return Ok(());
                }
                PermissionMode::Trust => {
                    tracing::info!(
                        "工具 {} 需要确认（Trust 模式），自动批准",
                        request.tool_name
                    );
                }
                PermissionMode::Yolo => {
                    // Yolo 模式下自动批准所有操作
                }
            }
        }

        // 执行工具
        let result = self.bridge.execute(&request).await;

        // 记录工具执行耗时（工具名 + 耗时），metrics 埋点失败不影响主流程
        AgentMetrics::global()
            .record_tool_execution_time(&request.tool_name, result.duration_ms as f64);
        if !result.success {
            AgentMetrics::global().record_error("tool_execution_failed");
        }

        // 将结果封装为消息，优先通过数据面 PUB 发送
        let tool_result_msg = FrameCodec::new_message(
            Topic::agent_tool_result(&request.agent_id),
            self.id.as_str(),
            &result,
        )?;
        if let Err(_) = transport.publish_data(&tool_result_msg).await {
            transport.send_message(&tool_result_msg).await?;
        }

        tracing::debug!(
            "工具执行完成：tool_call_id={}, success={}, duration={}ms",
            result.tool_call_id, result.success, result.duration_ms
        );
        Ok(())
    }
}

#[async_trait]
impl Node for ToolNode {
    fn node_type(&self) -> NodeType {
        NodeType::Tool
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Tool Node 启动：{} (工具数={}, 执行器数={}, 权限={:?})",
            self.id,
            self.bridge.tool_count(),
            self.bridge.executor_count(),
            self.permission_mode,
        );
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        match msg.topic.as_str() {
            t if t.ends_with("/tool_call") => {
                let request: ToolCallRequest = FrameCodec::decode_payload(&msg)?;
                tracing::debug!(
                    "Tool Node 收到工具调用：{} (agent={})",
                    request.tool_name, request.agent_id
                );
                self.handle_tool_call(request, transport).await?;
            }
            "sys/spawn" => {
                // 收到 Node 上线广播，广播工具列表给 Agent
                let tool_defs: Vec<crate::sampler::provider::ToolDefinition> = self.bridge
                    .tool_definitions()
                    .iter()
                    .map(|td| crate::sampler::provider::ToolDefinition {
                        name: td.name.clone(),
                        description: td.description.clone(),
                        parameters: td.parameters.clone(),
                    })
                    .collect();
                if !tool_defs.is_empty() {
                    let register_msg = FrameCodec::new_message(
                        Topic::tool_register(),
                        self.id.as_str(),
                        &tool_defs,
                    )?;
                    transport.send_message(&register_msg).await?;
                    tracing::info!("Tool Node 广播工具注册：{} 个工具", tool_defs.len());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            "agent/*/tool_call".into(),
            "sys/spawn".into(),
            "sys/shutdown".into(),
        ]
    }

    /// Tool 发布的 topic（数据面 PUB）
    fn published_topics(&self) -> Vec<String> {
        vec!["agent/*/tool_result".into(), "tool/register".into()]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        tracing::info!("Tool Node 关闭：{}", self.id);
        Ok(())
    }
}
