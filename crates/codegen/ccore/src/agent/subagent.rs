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
