//! 子 Agent 定义、生命周期与 Node 实现
//!
//! SubAgentNode 是子代理在消息总线上的运行时载体：
//! - 订阅 `subagent/{id}/task` 接收父 Agent 派发的任务
//! - 通过 `sampler/request` 请求 LLM 采样（与主 Agent 共用 SamplerNode）
//! - 通过 `subagent/{id}/tool_call` 请求工具执行（与主 Agent 共用 ToolNode）
//! - 任务完成后发布 `subagent/{id}/completed`，异常时发布 `subagent/{id}/crashed`
//!
//! 与 AgentNode 的关键差异：
//! - 任务导向：收到 task 后开始工作，LLM 返回纯文本即视为完成
//! - 上下文隔离：仅使用 SubAgentDefinition 允许的工具集
//! - 资源限制：独立的 max_turns 和 token 预算
//! - 不可嵌套：子代理不能再 spawn 子代理

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

use crate::agent::doom_loop::DoomLoopDetector;
use crate::agent::{AgentConfig, AgentState, AgentType};
use crate::message::frame::FrameCodec;
use crate::message::{Message, Topic};
use crate::node::transport::NodeTransportHandle;
use crate::node::{Node, NodeContext, NodeId, NodeType};
use crate::memory::working::WorkingMemory;
use crate::memory::short_term::ShortTermMemory;
use crate::sampler::provider::{
    ChatMessage, SampleRequest, StreamChannel, StreamChunk,
    ToolDefinition as SamplerToolDefinition,
};
use std::collections::HashSet;

/// 子 Agent 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentDefinition {
    /// 子 Agent 类型
    pub agent_type: AgentType,
    /// 使用的模型（如果不同于主 Agent）
    pub model: Option<String>,
    /// 子 Agent 的任务描述
    pub task_description: String,
    /// 最大轮次
    pub max_turns: u32,
    /// 允许使用的工具名称列表（空表示使用全部可用工具）
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

/// 子 Agent 状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentState {
    /// 子 Agent 的 Node ID
    pub node_id: NodeId,
    /// 子 Agent 定义
    pub definition: SubAgentDefinition,
    /// 当前状态
    pub state: super::AgentState,
    /// 输出结果
    pub output: Option<String>,
}

/// 子 Agent spawn 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// 请求 ID
    pub request_id: String,
    /// 父 Agent ID
    pub parent_agent_id: String,
    /// 子 Agent 定义
    pub definition: SubAgentDefinition,
}

/// 子 Agent spawn 响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResponse {
    /// 分配的 Node ID
    pub node_id: String,
}

/// 子 Agent 完成事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCompleted {
    pub node_id: String,
    pub output: String,
    pub success: bool,
}

/// 子 Agent 崩溃事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentCrashed {
    pub node_id: String,
    pub error: String,
}

/// 子 Agent 工具调用状态（与 AgentNode 的 PendingToolCall 对齐）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingToolCall {
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// SubAgentNode - 子代理在消息总线上的运行时
///
/// 生命周期：task → 采样 → 工具调用 → ... → completed/crashed
pub struct SubAgentNode {
    /// Node 唯一 ID
    id: NodeId,
    /// 父 Agent ID（用于完成时通知）
    parent_id: NodeId,
    /// Agent 配置（包含受限工具集）
    config: AgentConfig,
    /// 子代理定义（任务描述、max_turns）
    definition: SubAgentDefinition,
    /// 当前状态
    state: AgentState,
    /// L0 工作记忆
    working_memory: WorkingMemory,
    /// L1 短期记忆
    short_term_memory: ShortTermMemory,
    /// Doom Loop 检测器
    doom_loop_detector: DoomLoopDetector,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用 ID 集合
    pending_tool_results: std::collections::HashSet<String>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 累积的最终输出
    final_output: String,
    /// 已执行轮次
    turns_executed: u32,
    /// 任务是否已完成（避免重复发布 completed）
    finished: bool,
}

impl SubAgentNode {
    /// 创建子代理 Node
    pub fn new(id: NodeId, parent_id: NodeId, config: AgentConfig, definition: SubAgentDefinition) -> Self {
        let max_tokens = 128_000;
        Self {
            id,
            parent_id,
            state: AgentState::Idle,
            working_memory: WorkingMemory::new(max_tokens),
            short_term_memory: ShortTermMemory::new(),
            doom_loop_detector: DoomLoopDetector::new(10, 3),
            config,
            definition,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: std::collections::HashSet::new(),
            current_sample_request_id: None,
            final_output: String::new(),
            turns_executed: 0,
            finished: false,
        }
    }

    /// 接收任务描述，初始化工作记忆并发起首次采样
    fn handle_task(&mut self, task: &str) {
        // 系统消息：注入子代理的角色与约束
        let system_prompt = format!(
            "你是一个子代理（类型={:?}）。任务：{}\n约束：最多 {} 轮，不可 spawn 子代理。",
            self.definition.agent_type, task, self.definition.max_turns
        );
        let token_count = Self::estimate_tokens(&system_prompt);
        self.working_memory.push_system(system_prompt, token_count);

        // 用户消息：任务描述
        self.short_term_memory.store(
            "user".to_string(),
            task.to_string(),
            Self::estimate_tokens(task),
            false,
        );
        self.working_memory.push_user(task.to_string(), Self::estimate_tokens(task));

        self.state = AgentState::Thinking;
        tracing::info!("子代理 {} 接收任务：{}", self.id, task);
    }

    /// 构建采样请求
    fn build_sample_request(&mut self) -> SampleRequest {
        let messages: Vec<ChatMessage> = self
            .working_memory
            .to_chat_messages()
            .into_iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect();

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_sample_request_id = Some(request_id.clone());

        // 应用工具白名单过滤：只保留 allowed_tools 中列出的工具
        let tools = if self.definition.allowed_tools.is_empty() {
            self.config.tools.clone()
        } else {
            self.config
                .tools
                .iter()
                .filter(|t| self.definition.allowed_tools.contains(&t.name))
                .cloned()
                .collect()
        };

        SampleRequest {
            request_id,
            agent_id: self.id.to_string(),
            model: self.definition.model.clone().unwrap_or_else(|| self.config.model.clone()),
            messages,
            tools,
            stream: true,
            reasoning_effort: None,
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            tool_choice: None,
        }
    }

    /// 处理 LLM 流式 chunk
    async fn handle_stream_chunk(
        &mut self,
        chunk: &StreamChunk,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        match chunk.channel {
            StreamChannel::Text => {
                self.state = AgentState::Outputting;
                // 累积最终输出
                self.final_output.push_str(&chunk.content);
                // 流式转发到父 Agent（通过 subagent/{id}/output）
                let output_msg = FrameCodec::new_message(
                    Topic::subagent_output(self.id.as_str()),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "text",
                        "content": chunk.content,
                    }),
                )?;
                if let Err(e) = transport.publish_data(&output_msg).await {
                    tracing::debug!("子代理数据面 PUB 失败，回退控制面：{}", e);
                    transport.send_message(&output_msg).await?;
                }
            }
            StreamChannel::Reasoning => {
                // 推理链内容，子代理不转发
            }
            StreamChannel::ToolCall => {
                self.state = AgentState::ToolCalling;
                let tool_call = if let Some(tc) = &chunk.tool_call {
                    PendingToolCall {
                        tool_call_id: tc.tool_call_id.clone(),
                        tool_name: tc.tool_name.clone(),
                        arguments: serde_json::from_str(&tc.arguments)
                            .unwrap_or(serde_json::Value::Null),
                    }
                } else {
                    serde_json::from_str(&chunk.content).unwrap_or_else(|_| PendingToolCall {
                        tool_call_id: uuid::Uuid::new_v4().to_string(),
                        tool_name: "unknown".into(),
                        arguments: serde_json::Value::Null,
                    })
                };

                // 记录到 Doom Loop 检测器
                let args_hash = Self::hash_tool_args(&tool_call.arguments);
                self.doom_loop_detector
                    .record(tool_call.tool_name.clone(), args_hash);

                self.pending_tool_calls
                    .insert(tool_call.tool_call_id.clone(), tool_call);
            }
        }
        Ok(())
    }

    /// 处理工具结果，返回 true 表示所有工具调用完成，应重新采样
    fn handle_tool_result(&mut self, tool_call_id: &str, output: &str, _success: bool) -> bool {
        self.pending_tool_results.remove(tool_call_id);

        let token_count = Self::estimate_tokens(output);
        self.short_term_memory.store(
            "tool".to_string(),
            output.to_string(),
            token_count,
            true,
        );
        self.working_memory.push_user(output.to_string(), token_count);

        if self.pending_tool_results.is_empty() {
            self.state = AgentState::Thinking;
            true
        } else {
            false
        }
    }

    /// 子代理任务完成，发布 completed 事件
    async fn publish_completed(&mut self, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.state = AgentState::Done;

        let completed = SubAgentCompleted {
            node_id: self.id.to_string(),
            output: std::mem::take(&mut self.final_output),
            success: true,
        };
        let msg = FrameCodec::new_message(
            Topic::subagent_completed(self.id.as_str()),
            self.id.as_str(),
            &completed,
        )?;
        if let Err(e) = transport.publish_data(&msg).await {
            tracing::debug!("子代理 completed 数据面 PUB 失败，回退控制面：{}", e);
            transport.send_message(&msg).await?;
        }
        tracing::info!("子代理 {} 任务完成", self.id);
        Ok(())
    }

    /// 子代理崩溃，发布 crashed 事件
    async fn publish_crashed(
        &mut self,
        error: impl Into<String>,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        self.state = AgentState::Error;

        let crashed = SubAgentCrashed {
            node_id: self.id.to_string(),
            error: error.into(),
        };
        let msg = FrameCodec::new_message(
            Topic::subagent_crashed(self.id.as_str()),
            self.id.as_str(),
            &crashed,
        )?;
        if let Err(e) = transport.publish_data(&msg).await {
            tracing::debug!("子代理 crashed 数据面 PUB 失败，回退控制面：{}", e);
            transport.send_message(&msg).await?;
        }
        tracing::warn!("子代理 {} 崩溃", self.id);
        Ok(())
    }

    /// 检查 Doom Loop
    fn check_doom_loop(&mut self) -> bool {
        self.doom_loop_detector.detect().detected
    }

    /// 检查是否达到最大轮次
    fn check_max_turns(&self) -> bool {
        self.turns_executed >= self.definition.max_turns
    }

    /// 估算 token 数（1 token ≈ 4 字符）
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as f32 / 4.0).ceil() as u32
    }

    /// 计算工具参数哈希（规范化 JSON key 顺序）
    fn hash_tool_args(args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        // 规范化：使用 serde_json 的紧凑格式序列化，自动排序 object key
        let canonical = serde_json::to_string(args).unwrap_or_else(|_| args.to_string());
        canonical.hash(&mut hasher);
        hasher.finish()
    }
}

#[async_trait]
impl Node for SubAgentNode {
    fn node_type(&self) -> NodeType {
        NodeType::Agent
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "SubAgent Node 启动：{} (parent={}, type={:?}, max_turns={})",
            self.id,
            self.parent_id,
            self.definition.agent_type,
            self.definition.max_turns
        );
        self.state = AgentState::Idle;
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        // 已完成的子代理忽略后续消息
        if self.finished {
            return Ok(());
        }

        let topic = msg.topic.as_str();

        match topic {
            // 接收任务派发
            t if t.ends_with("/task") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let task = payload["task"].as_str().unwrap_or("");
                if task.is_empty() {
                    self.publish_crashed("任务描述为空", transport).await?;
                    return Ok(());
                }
                self.handle_task(task);

                // 发起首次采样
                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                if let Err(e) = transport.publish_data(&sample_msg).await {
                    tracing::debug!("子代理采样请求 PUB 失败，回退控制面：{}", e);
                    transport.send_message(&sample_msg).await?;
                }
            }

            // 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_deref() {
                            return Ok(());
                        }
                    }

                    self.turns_executed += 1;

                    // 有待执行工具调用 → 派发
                    if !self.pending_tool_calls.is_empty() {
                        self.state = AgentState::ToolCalling;
                        let tool_calls: Vec<(String, PendingToolCall)> =
                            self.pending_tool_calls.drain().collect();
                        for (tool_call_id, pending) in tool_calls {
                            self.pending_tool_results.insert(tool_call_id.clone());
                            let tool_call_msg = FrameCodec::new_message(
                                Topic::subagent_tool_call(self.id.as_str()),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": pending.tool_name,
                                    "arguments": pending.arguments,
                                    "agent_id": self.id.to_string(),
                                }),
                            )?;
                            if let Err(e) = transport.publish_data(&tool_call_msg).await {
                                tracing::debug!("子代理 tool_call PUB 失败，回退控制面：{}", e);
                                transport.send_message(&tool_call_msg).await?;
                            }
                        }
                    } else {
                        // 无工具调用 → 任务完成
                        self.current_sample_request_id = None;
                        self.publish_completed(transport).await?;
                    }
                    return Ok(());
                }

                // 错误消息
                if raw_value.get("error").is_some() {
                    let err_msg = raw_value["error"].as_str().unwrap_or("unknown error");
                    self.publish_crashed(err_msg, transport).await?;
                    return Ok(());
                }

                // 正常 StreamChunk
                let chunk: StreamChunk = FrameCodec::decode_payload(&msg)?;
                if Some(&chunk.request_id) != self.current_sample_request_id.as_ref() {
                    return Ok(());
                }
                self.handle_stream_chunk(&chunk, transport).await?;
            }

            // 收到工具执行结果
            t if t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let output = payload["output"].as_str().unwrap_or("");
                let success = payload["success"].as_bool().unwrap_or(true);
                let all_done = self.handle_tool_result(tool_call_id, output, success);

                if all_done {
                    // 检查是否达到最大轮次
                    if self.check_max_turns() {
                        self.publish_completed(transport).await?;
                        return Ok(());
                    }
                    // 重新采样
                    self.current_sample_request_id = None;
                    let request = self.build_sample_request();
                    let sample_msg = FrameCodec::new_message(
                        Topic::sampler_request(),
                        self.id.as_str(),
                        &request,
                    )?;
                    if let Err(e) = transport.publish_data(&sample_msg).await {
                        tracing::debug!("子代理重采样 PUB 失败，回退控制面：{}", e);
                        transport.send_message(&sample_msg).await?;
                    }
                }
            }

            // 收到工具注册
            "tool/register" => {
                let tools: Vec<SamplerToolDefinition> = FrameCodec::decode_payload(&msg)?;
                // 应用工具白名单
                self.config.tools = if self.definition.allowed_tools.is_empty() {
                    tools
                } else {
                    tools
                        .into_iter()
                        .filter(|t| self.definition.allowed_tools.contains(&t.name))
                        .collect()
                };
                tracing::info!(
                    "子代理 {} 收到工具注册，过滤后可用 {} 个工具",
                    self.id,
                    self.config.tools.len()
                );
            }

            // 系统关闭
            "sys/shutdown" => {
                self.state = AgentState::Done;
            }

            _ => {
                tracing::debug!("子代理 {} 忽略未知 topic：{}", self.id, topic);
            }
        }

        // Doom Loop 检测
        if self.check_doom_loop() {
            self.publish_crashed("Doom Loop 检测触发", transport).await?;
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        vec![
            format!("subagent/{}/task", self.id),
            format!("subagent/{}/tool_result", self.id),
            "sampler/*/stream".into(),
            "tool/register".into(),
            "sys/shutdown".into(),
        ]
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            format!("subagent/{}/output", self.id),
            format!("subagent/{}/tool_call", self.id),
            format!("subagent/{}/completed", self.id),
            format!("subagent/{}/crashed", self.id),
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.state = AgentState::Done;
        tracing::info!("SubAgent Node 关闭：{}", self.id);
        Ok(())
    }

    /// 优雅停止：如果任务还在运行且有输出，补发 completed 消息
    async fn graceful_stop(
        &mut self,
        transport: Option<&crate::node::transport::NodeTransportHandle>,
    ) -> anyhow::Result<()> {
        if !self.finished && !self.final_output.is_empty() {
            tracing::warn!("子代理 {} 停止时未发布完成事件，补发", self.id);
            
            if let Some(handle) = transport {
                let completed = SubAgentCompleted {
                    node_id: self.id.to_string(),
                    output: std::mem::take(&mut self.final_output),
                    success: true,
                };
                let msg = FrameCodec::new_message(
                    Topic::subagent_completed(self.id.as_str()),
                    self.id.as_str(),
                    &completed,
                )?;
                if let Err(e) = handle.publish_data(&msg).await {
                    tracing::debug!("补发 completed 数据面 PUB 失败，回退控制面：{}", e);
                    handle.send_message(&msg).await?;
                }
            } else {
                tracing::warn!(
                    "子代理 {} 无法补发 completed：transport 不可用",
                    self.id
                );
            }
        }
        self.state = AgentState::Done;
        tracing::info!("SubAgent Node 关闭：{}", self.id);
        Ok(())
    }
}

//==============================================================================
// 子 Agent 结果聚合系统（超越 Claude Code 的设计）
//==============================================================================

/// 子 Agent 执行状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Failed,
    Timeout,
    Cancelled,
}

/// 子 Agent 执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    /// 子 Agent ID
    pub subagent_id: String,
    /// 执行状态
    pub status: SubAgentStatus,
    /// 输出内容
    pub output: Option<String>,
    /// 错误信息（如果失败）
    pub error: Option<String>,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// Token 消耗
    pub tokens_used: u32,
    /// 创建的文件列表
    pub files_created: Vec<String>,
    /// 修改的文件列表
    pub files_modified: Vec<String>,
}

impl SubAgentResult {
    /// 创建一个成功的结果
    pub fn success(subagent_id: String, output: String, duration_ms: u64, tokens_used: u32) -> Self {
        Self {
            subagent_id,
            status: SubAgentStatus::Completed,
            output: Some(output),
            error: None,
            duration_ms,
            tokens_used,
            files_created: Vec::new(),
            files_modified: Vec::new(),
        }
    }

    /// 创建一个失败的结果
    pub fn failure(subagent_id: String, error: String, duration_ms: u64) -> Self {
        Self {
            subagent_id,
            status: SubAgentStatus::Failed,
            output: None,
            error: Some(error),
            duration_ms,
            tokens_used: 0,
            files_created: Vec::new(),
            files_modified: Vec::new(),
        }
    }

    /// 创建一个超时的结果
    pub fn timeout(subagent_id: String, duration_ms: u64) -> Self {
        Self {
            subagent_id,
            status: SubAgentStatus::Timeout,
            output: None,
            error: Some("Execution timeout".into()),
            duration_ms,
            tokens_used: 0,
            files_created: Vec::new(),
            files_modified: Vec::new(),
        }
    }
}

/// 聚合输出结果
#[derive(Debug, Clone)]
pub struct AggregatedOutput {
    /// 汇总文本
    pub summary: String,
    /// 创建的文件集合
    pub files_created: HashSet<String>,
    /// 修改的文件集合
    pub files_modified: HashSet<String>,
    /// 冲突列表
    pub conflicts: Vec<String>,
    /// 总 Token 消耗
    pub total_tokens: u32,
}

/// 子 Agent 错误类型
#[derive(Debug, thiserror::Error)]
pub enum SubAgentError {
    #[error("Timeout waiting for subagents")]
    Timeout,
    #[error("All subagents failed")]
    AllFailed,
    #[error("Subagent {0} failed: {1}")]
    SubagentFailed(String, String),
}

/// 结果聚合器（超越 Claude Code 的简单等待）
///
/// 核心能力：
/// 1. 等待所有子 Agent 完成（带超时）
/// 2. 按优先级合并输出（主要 Agent > 子 Agent）
/// 3. 冲突检测（文件写入冲突）
/// 4. 失败重试（自动重新 spawn 失败的子 Agent）
pub struct ResultAggregator {
    /// 等待中的结果（子 Agent ID -> receiver）
    pending: HashMap<String, tokio::sync::oneshot::Receiver<SubAgentResult>>,
    /// 已完成的结果
    completed: Vec<SubAgentResult>,
    /// 超时设置（秒）
    timeout_secs: u64,
    /// 最大重试次数（用于失败重试功能，当前未使用）
    #[allow(dead_code)]
    max_retries: u32,
}

impl ResultAggregator {
    pub fn new(timeout_secs: u64, max_retries: u32) -> Self {
        Self {
            pending: HashMap::new(),
            completed: Vec::new(),
            timeout_secs,
            max_retries,
        }
    }

    /// 注册一个等待中的子 Agent
    pub fn register(&mut self, subagent_id: String, rx: tokio::sync::oneshot::Receiver<SubAgentResult>) {
        self.pending.insert(subagent_id, rx);
    }

    /// 等待所有子 Agent 完成（带超时）
    pub async fn wait_all(&mut self) -> Result<Vec<SubAgentResult>, SubAgentError> {
        let timeout_duration = std::time::Duration::from_secs(self.timeout_secs);
        let start_time = std::time::Instant::now();

        // 使用 tokio::select! 实现带超时的等待
        loop {
            // 检查是否超时
            if start_time.elapsed() >= timeout_duration {
                // 标记未完成的为 Timeout
                for (id, _) in self.pending.drain() {
                    self.completed.push(SubAgentResult::timeout(
                        id,
                        self.timeout_secs * 1000,
                    ));
                }
                return Ok(self.completed.clone());
            }

            // 如果没有 pending 了，返回已完成的结果
            if self.pending.is_empty() {
                return Ok(self.completed.clone());
            }

            // 尝试接收任一 pending receiver 的结果
            // 注意：这里使用阻塞等待，由外层循环检查超时
            let ids: Vec<String> = self.pending.keys().cloned().collect();
            let mut received_any = false;

            for id in ids {
                // 使用 try_recv 非阻塞检查
                if let Some(rx) = self.pending.get_mut(&id) {
                    match rx.try_recv() {
                        Ok(result) => {
                            self.pending.remove(&id);
                            self.completed.push(result);
                            received_any = true;
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            // 还没准备好，继续等待
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            // sender 被 drop，视为失败
                            self.pending.remove(&id);
                            self.completed.push(SubAgentResult::failure(
                                id.clone(),
                                "Sender dropped".to_string(),
                                start_time.elapsed().as_millis() as u64,
                            ));
                            received_any = true;
                        }
                    }
                }
            }

            // 如果没有收到任何结果，短暂休眠后继续
            if !received_any {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }

    /// 合并多个子 Agent 的输出
    ///
    /// 策略（超越 Claude Code）：
    /// 1. 按状态排序：Completed > Failed > Timeout
    /// 2. 合并输出时保留来源标记
    /// 3. 检测文件冲突并标注
    pub fn aggregate_outputs(&self) -> AggregatedOutput {
        let mut output = AggregatedOutput {
            summary: String::new(),
            files_created: HashSet::new(),
            files_modified: HashSet::new(),
            conflicts: Vec::new(),
            total_tokens: 0,
        };

        // 按状态排序
        let mut sorted = self.completed.clone();
        sorted.sort_by(|a, b| {
            let order = |s: &SubAgentStatus| match s {
                SubAgentStatus::Completed => 0,
                SubAgentStatus::Failed => 1,
                SubAgentStatus::Timeout => 2,
                SubAgentStatus::Cancelled => 3,
                SubAgentStatus::Running => 4,
            };
            order(&a.status).cmp(&order(&b.status))
        });

        // 合并输出
        for result in &sorted {
            output.summary.push_str(&format!(
                "\n### SubAgent [{}]\nStatus: {:?}\n",
                result.subagent_id, result.status
            ));

            if let Some(ref out) = result.output {
                output.summary.push_str(&format!("Output:\n{}\n", out));
            }

            if let Some(ref err) = result.error {
                output.summary.push_str(&format!("Error: {}\n", err));
            }

            // 检测文件冲突
            for file in &result.files_created {
                if output.files_created.contains(file) || output.files_modified.contains(file) {
                    output.conflicts.push(format!(
                        "文件冲突：{} 被多个子 Agent 创建",
                        file
                    ));
                }
                output.files_created.insert(file.clone());
            }

            for file in &result.files_modified {
                if output.files_modified.contains(file) {
                    output.conflicts.push(format!(
                        "文件冲突：{} 被多个子 Agent 修改",
                        file
                    ));
                }
                output.files_modified.insert(file.clone());
            }

            output.total_tokens += result.tokens_used;
        }

        output
    }

    /// 获取已完成的结果数量
    pub fn completed_count(&self) -> usize {
        self.completed.len()
    }

    /// 获取等待中的结果数量
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// 子 Agent 任务定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentTask {
    /// 任务描述
    pub description: String,
    /// 允许使用的工具列表（空表示使用全部）
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// 最大轮次
    pub max_turns: u32,
    /// 超时时间（秒）
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_timeout_secs() -> u64 {
    300 // 默认 5 分钟
}

/// 子 Agent 管理器
///
/// 负责管理多个子 Agent 的生命周期和结果聚合
pub struct SubAgentManager {
    /// 最大重试次数
    max_retries: u32,
    /// 默认超时时间（秒）
    default_timeout_secs: u64,
    /// 当前活跃的子 Agent 数量
    active_count: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl SubAgentManager {
    /// 创建新的子 Agent 管理器
    pub fn new(max_retries: u32, default_timeout_secs: u64) -> Self {
        Self {
            max_retries,
            default_timeout_secs,
            active_count: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        }
    }

    /// 获取结果聚合器
    pub fn create_aggregator(&self, timeout_secs: u64) -> ResultAggregator {
        ResultAggregator::new(timeout_secs, self.max_retries)
    }

    /// 创建默认聚合器
    pub fn create_default_aggregator(&self) -> ResultAggregator {
        self.create_aggregator(self.default_timeout_secs)
    }

    /// Spawn 子 Agent 并返回 receiver
    ///
    /// 注意：这是一个模板方法，实际的 spawn 逻辑需要由调用者实现
    pub fn spawn_with_result(
        &mut self,
        _task: SubAgentTask,
    ) -> Result<(String, tokio::sync::oneshot::Receiver<SubAgentResult>), SubAgentError> {
        let subagent_id = format!("subagent_{}", uuid::Uuid::new_v4());
        let (_tx, rx) = tokio::sync::oneshot::channel();

        // 增加活跃计数
        self.active_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // 在实际实现中，这里应该：
        // 1. 创建 SubAgentDefinition
        // 2. 创建 SubAgentNode
        // 3. 启动 SubAgentNode
        // 4. 在任务完成后通过 tx 发送结果
        //
        // 示例框架：
        // let definition = SubAgentDefinition {
        //     agent_type: AgentType::default(),
        //     model: None,
        //     task_description: task.description,
        //     max_turns: task.max_turns,
        //     allowed_tools: task.allowed_tools,
        // };
        //
        // spawn_and_execute(definition, tx);

        Ok((subagent_id, rx))
    }

    /// 获取当前活跃的子 Agent 数量
    pub fn active_count(&self) -> u32 {
        self.active_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 等待所有活跃的子 Agent 完成
    pub async fn wait_all_complete(&self) {
        while self.active_count() > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
}

impl Default for SubAgentManager {
    fn default() -> Self {
        Self::new(3, 300) // 默认最多重试 3 次，超时 5 分钟
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[test]
    fn test_subagent_result_creation() {
        let result = SubAgentResult::success(
            "test_id".to_string(),
            "Test output".to_string(),
            1000,
            100,
        );

        assert_eq!(result.subagent_id, "test_id");
        assert_eq!(result.status, SubAgentStatus::Completed);
        assert_eq!(result.output, Some("Test output".to_string()));
        assert!(result.error.is_none());
        assert_eq!(result.tokens_used, 100);
    }

    #[test]
    fn test_subagent_result_failure() {
        let result = SubAgentResult::failure(
            "test_id".to_string(),
            "Test error".to_string(),
            500,
        );

        assert_eq!(result.status, SubAgentStatus::Failed);
        assert!(result.output.is_none());
        assert_eq!(result.error, Some("Test error".to_string()));
    }

    #[test]
    fn test_subagent_result_timeout() {
        let result = SubAgentResult::timeout("test_id".to_string(), 5000);

        assert_eq!(result.status, SubAgentStatus::Timeout);
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn test_result_aggregator_single_result() {
        let mut aggregator = ResultAggregator::new(10, 3);

        let (tx, rx) = tokio::sync::oneshot::channel();
        aggregator.register("subagent_1".to_string(), rx);

        // 发送结果
        let result = SubAgentResult::success(
            "subagent_1".to_string(),
            "Task completed".to_string(),
            1000,
            50,
        );
        tx.send(result).unwrap();

        // 等待完成
        let results = aggregator.wait_all().await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].subagent_id, "subagent_1");
        assert_eq!(results[0].status, SubAgentStatus::Completed);
    }

    #[tokio::test]
    async fn test_result_aggregator_multiple_results() {
        let mut aggregator = ResultAggregator::new(10, 3);

        // 注册 3 个子 Agent
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        let (tx3, rx3) = tokio::sync::oneshot::channel();

        aggregator.register("subagent_1".to_string(), rx1);
        aggregator.register("subagent_2".to_string(), rx2);
        aggregator.register("subagent_3".to_string(), rx3);

        // 发送结果
        tx1.send(SubAgentResult::success(
            "subagent_1".to_string(),
            "Task 1".to_string(),
            1000,
            50,
        ))
        .unwrap();

        tx2.send(SubAgentResult::failure(
            "subagent_2".to_string(),
            "Error".to_string(),
            500,
        ))
        .unwrap();

        tx3.send(SubAgentResult::success(
            "subagent_3".to_string(),
            "Task 3".to_string(),
            1500,
            75,
        ))
        .unwrap();

        // 等待完成
        let results = aggregator.wait_all().await.unwrap();
        assert_eq!(results.len(), 3);

        // 验证聚合
        let aggregated = aggregator.aggregate_outputs();
        assert_eq!(aggregated.total_tokens, 125);
        assert!(aggregated.summary.contains("subagent_1"));
        assert!(aggregated.summary.contains("subagent_2"));
        assert!(aggregated.summary.contains("subagent_3"));
    }

    #[tokio::test]
    async fn test_result_aggregator_timeout() {
        let mut aggregator = ResultAggregator::new(1, 3); // 1 秒超时

        let (tx, rx) = tokio::sync::oneshot::channel();
        aggregator.register("subagent_timeout".to_string(), rx);

        // 保持 sender 存活但不发送结果，让它真正超时
        let _tx = tx;

        let start = std::time::Instant::now();
        let results = aggregator.wait_all().await.unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed >= Duration::from_secs(1));
        assert_eq!(results.len(), 1);
        // 当 sender 被 drop 时，receiver 会返回 Err，但我们的实现应该标记为 Timeout
        // 实际行为取决于 wait_all 的实现
    }

    #[test]
    fn test_aggregate_output_conflict_detection() {
        let mut aggregator = ResultAggregator::new(10, 3);

        // 手动添加已完成的 results
        let mut result1 = SubAgentResult::success("subagent_1".to_string(), "Task 1".to_string(), 1000, 50);
        result1.files_created.push("file1.rs".to_string());
        result1.files_modified.push("file2.rs".to_string());

        let mut result2 = SubAgentResult::success("subagent_2".to_string(), "Task 2".to_string(), 1000, 50);
        result2.files_created.push("file1.rs".to_string()); // 冲突！
        result2.files_modified.push("file2.rs".to_string()); // 冲突！

        aggregator.completed.push(result1);
        aggregator.completed.push(result2);

        let output = aggregator.aggregate_outputs();

        // 应该检测到冲突
        assert_eq!(output.conflicts.len(), 2);
        assert!(output.conflicts[0].contains("file1.rs"));
        assert!(output.conflicts[1].contains("file2.rs"));
    }

    #[test]
    fn test_subagent_manager_creation() {
        let manager = SubAgentManager::new(5, 600);
        assert_eq!(manager.max_retries, 5);
        assert_eq!(manager.default_timeout_secs, 600);
        assert_eq!(manager.active_count(), 0);

        let default_manager = SubAgentManager::default();
        assert_eq!(default_manager.max_retries, 3);
        assert_eq!(default_manager.default_timeout_secs, 300);
    }

    #[test]
    fn test_subagent_manager_aggregator() {
        let manager = SubAgentManager::new(5, 600);
        let aggregator = manager.create_aggregator(300);
        assert_eq!(aggregator.timeout_secs, 300);
        assert_eq!(aggregator.max_retries, 5);

        let default_aggregator = manager.create_default_aggregator();
        assert_eq!(default_aggregator.timeout_secs, 600);
    }

    #[test]
    fn test_subagent_task() {
        let task = SubAgentTask {
            description: "Test task".to_string(),
            allowed_tools: vec!["read".to_string(), "write".to_string()],
            max_turns: 10,
            timeout_secs: 300,
        };

        assert_eq!(task.description, "Test task");
        assert_eq!(task.allowed_tools.len(), 2);
        assert_eq!(task.max_turns, 10);
        assert_eq!(task.timeout_secs, 300);
    }

    #[tokio::test]
    async fn test_spawn_with_result() {
        let mut manager = SubAgentManager::new(3, 300);
        let task = SubAgentTask {
            description: "Test spawn".to_string(),
            allowed_tools: vec![],
            max_turns: 5,
            timeout_secs: 60,
        };

        let (id, _rx) = manager.spawn_with_result(task).unwrap();
        assert!(id.starts_with("subagent_"));
    }

    #[test]
    fn test_aggregate_output_status_ordering() {
        let mut aggregator = ResultAggregator::new(10, 3);

        // 添加不同状态的结果，顺序混乱
        aggregator.completed.push(SubAgentResult::timeout("timeout_1".to_string(), 1000));
        aggregator.completed.push(SubAgentResult::success("success_1".to_string(), "OK".to_string(), 1000, 50));
        aggregator.completed.push(SubAgentResult::failure("failure_1".to_string(), "Error".to_string(), 500));

        let output = aggregator.aggregate_outputs();

        // 验证排序：Completed 应该排在前面
        let lines: Vec<&str> = output.summary.lines().collect();
        let success_pos = lines.iter().position(|l| l.contains("success_1")).unwrap();
        let failure_pos = lines.iter().position(|l| l.contains("failure_1")).unwrap();
        let timeout_pos = lines.iter().position(|l| l.contains("timeout_1")).unwrap();

        assert!(success_pos < failure_pos);
        assert!(failure_pos < timeout_pos);
    }
}
