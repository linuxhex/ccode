//! Thinker Node — 大脑皮层（仿生架构，路线 A：感官内置）
//!
//! Fusion: 唯一决策 Node（不是上帝进程）。
//! - 拥有：感知(Ear/Eye/Nose/Skin 内置)、agentic loop、doom-loop、max_turns、goal
//! - 不拥有：工具执行、JSONL 持久化实现、LLM HTTP
//! - 协作：sampler/request、agent/{id}/tool_call、state/persist|query|compact、agent/{id}/output
//!
//! ThinkerNode 是 AgentNode 的仿生架构演进版，核心改进：
//! 1. 感官层内置：Eye/Ear/Nose/Skin 作为内部方法，不拆独立进程
//! 2. 运动层走 ToolNode：工具调用通过 agent/{id}/tool_call 发给 ToolNode
//! 3. 输出层走 TUINode：文本输出通过 agent/{id}/output 发给 TUINode
//!
//! 仿生隐喻保留在概念层面：
//! - observe()：视觉（Eye）— 观察 tool 结果中的文件内容
//! - listen()：听觉（Ear）— 监听用户输入
//! - sniff()：嗅觉（Nose）— 解析编译错误/代码异味
//! - feel()：触觉（Skin）— 感知工具执行反馈
//!
//! 通信路径（与 AgentNode 完全兼容）：
//! - 输入：agent/{id}/input ← TUINode
//! - 采样：sampler/request → SamplerNode → sampler/*/stream
//! - 工具调用：agent/{id}/tool_call → ToolNode → agent/{id}/tool_result
//! - 输出：agent/{id}/output → TUINode
//! - 取消：agent/{id}/cancel ← AcpNode/TUINode
//!
//! 核心循环：
//! 1. 收到 input → listen() → 构建 L0 工作记忆 → 发送 sampler/request
//! 2. 收到 LLM 流式响应 → 解析 tool_call 或 text
//! 3. 如果有 tool_call → 发送到 agent/{id}/tool_call → 收到 tool_result → feel() + sniff() → 回到步骤 2
//! 4. 如果是纯文本 → 发送到 agent/{id}/output → 等待下一轮 input
//! 5. 每轮结束执行滑动窗口更新
//! 6. Doom Loop 检测：重复工具调用超过阈值则终止

use async_trait::async_trait;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::message::frame::FrameCodec;
use crate::message::Message;
use crate::message::Topic;
use crate::metrics::AgentMetrics;
use crate::node::{Node, NodeId, NodeType, NodeContext};
use crate::node::agent::PendingToolCall;
use crate::node::transport::NodeTransportHandle;
use crate::agent::{AgentConfig, AgentState};
use crate::agent::doom_loop::{DoomLoopDetector, DoomLoopResult, EscapeAction};
use crate::agent::loop_state::{LoopStateMachine, LoopEvent, LoopAction, ToolExecutionOutcome};
use crate::agent::orchestrator::Orchestrator;
use crate::agent::subagent::SubAgentCrashed;
use crate::memory::working::{WorkingMemory, MessageRole};
use crate::memory::short_term::ShortTermMemory;
use crate::memory::window::SlidingWindow;
use crate::sampler::provider::{
    SampleRequest, ChatMessage, StreamChunk, StreamChannel, ToolDefinition as SamplerToolDefinition,
};

/// 感官信号缓冲最大容量
const SENSORY_BUFFER_CAPACITY: usize = 20;

/// 感官信号（内部使用，不经消息总线）
#[derive(Debug, Clone)]
pub struct SensorySignal {
    /// 来源器官（如 "nose", "skin", "eye"）
    pub source_organ: String,
    /// 信号类型（如 "compile_error", "touch"）
    pub signal_type: String,
    /// 供 LLM 理解的摘要
    pub summary: String,
    /// 严重程度（"info", "warning", "error"）
    pub severity: String,
}

/// Thinker Node 实现（仿生架构，路线 A：感官内置）
///
/// 主循环由 LoopStateMachine 驱动（借鉴 Claude Code queryLoop 设计），
/// 状态变迁通过 transition() 显式执行，可观测、可中断、可恢复。
pub struct ThinkerNode {
    /// Node 唯一 ID
    id: NodeId,
    /// Agent 配置（复用 AgentConfig）
    config: AgentConfig,
    /// 当前状态（由 LoopStateMachine 驱动）
    state: AgentState,
    /// 主循环状态机（借鉴 Claude Code，驱动 Idle→Thinking→ToolCalling→Done 变迁）
    loop_state_machine: LoopStateMachine,
    /// L0 工作记忆
    working_memory: WorkingMemory,
    /// L1 短期记忆
    short_term_memory: ShortTermMemory,
    /// 子 Agent 编排器
    orchestrator: Orchestrator,
    /// Doom Loop 检测器
    doom_loop_detector: DoomLoopDetector,
    /// 下一轮需禁用的工具名（Doom Loop 逃脱：仅禁用一轮）
    disabled_tool_next_round: Option<String>,
    /// 滑动窗口更新器
    #[allow(dead_code)]
    sliding_window: SlidingWindow,
    /// 等待中的工具调用（tool_call_id → PendingToolCall）
    pending_tool_calls: HashMap<String, PendingToolCall>,
    /// 正在执行中的工具调用（tool_call_id → tool_name，等待结果返回）
    pending_tool_results: HashMap<String, String>,
    /// 当前 LLM 采样请求 ID
    current_sample_request_id: Option<String>,
    /// 当前推理周期的开始时间
    inference_start: Option<std::time::Instant>,
    /// 已使用的 token 数
    #[allow(dead_code)]
    tokens_used: u32,
    /// 已执行轮次
    turns_executed: u32,
    /// 感官信号缓冲（最近 20 条，内置处理）
    sensory_buffer: Vec<SensorySignal>,
    /// 取消请求标志（收到 agent/{id}/cancel 后设置）
    cancel_requested: bool,
}

impl ThinkerNode {
    pub fn new(id: NodeId, config: AgentConfig) -> Self {
        let max_tokens = 128_000;
        Self {
            id,
            state: AgentState::Idle,
            loop_state_machine: LoopStateMachine::new(),
            working_memory: WorkingMemory::new(max_tokens),
            short_term_memory: ShortTermMemory::new(),
            orchestrator: Orchestrator::new(10),
            doom_loop_detector: DoomLoopDetector::new(10, 3),
            disabled_tool_next_round: None,
            sliding_window: SlidingWindow::new(max_tokens),
            config,
            pending_tool_calls: HashMap::new(),
            pending_tool_results: HashMap::new(),
            current_sample_request_id: None,
            inference_start: None,
            tokens_used: 0,
            turns_executed: 0,
            sensory_buffer: Vec::with_capacity(SENSORY_BUFFER_CAPACITY),
            cancel_requested: false,
        }
    }

    // ── 内置感官模块（路线 A：不拆独立进程） ──

    /// 听觉（Ear）：处理用户输入
    ///
    /// 将用户输入存入工作记忆，标记为 Thinking 状态
    fn listen(&mut self, content: &str, role: &str) {
        let _entry_id = self.short_term_memory.store(
            role.to_string(),
            content.to_string(),
            Self::estimate_tokens(content),
            false,
        );

        self.working_memory.push_hot(
            MessageRole::try_from(role).unwrap_or(MessageRole::User),
            content.to_string(),
            Self::estimate_tokens(content),
        );

        self.state = AgentState::Thinking;
        tracing::debug!("Thinker {} 听到输入，轮次 {}", self.id, self.turns_executed);
    }

    /// 触觉（Skin）：感知工具执行结果
    ///
    /// 解析工具输出，提取关键信息注入工作记忆。
    /// 对于编译/检查类工具输出，自动调用 sniff() 解析错误。
    /// 同时通过消息总线向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    async fn feel(&mut self, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) {
        let summary = if success {
            format!("[工具 {} 执行成功]", tool_name)
        } else {
            format!("[工具 {} 执行失败]", tool_name)
        };

        // 记录触觉信号
        self.push_sensory(SensorySignal {
            source_organ: "skin".into(),
            signal_type: "tool_result".into(),
            summary: summary.clone(),
            severity: if success { "info" } else { "error" }.into(),
        });

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/skin/{}", tool_name),
            &serde_json::json!({
                "tool_name": tool_name,
                "success": success,
                "output_preview": output.chars().take(200).collect::<String>(),
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送感官信号到 Kernel 失败：{}", e);
        }

        // 如果是编译/检查类工具，自动嗅探
        if !success && Self::is_compile_related_tool(tool_name) {
            self.sniff(output, transport).await;
        }
    }

    /// 嗅觉（Nose）：解析编译错误和代码异味
    ///
    /// 从工具输出中提取错误信息，格式化后注入工作记忆。
    /// 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入。
    /// 编译错误格式：error[E0xxx]: message
    async fn sniff(&mut self, output: &str, transport: &NodeTransportHandle) {
        let error_lines: Vec<&str> = output
            .lines()
            .filter(|line| line.contains("error[") || line.contains("error:"))
            .take(5) // 最多取 5 条错误
            .collect();

        if error_lines.is_empty() {
            return;
        }

        let summary = format!("编译/检查发现 {} 个错误：\n{}", error_lines.len(), error_lines.join("\n"));

        self.push_sensory(SensorySignal {
            source_organ: "nose".into(),
            signal_type: "compile_error".into(),
            summary: summary.clone(),
            severity: "error".into(),
        });

        // 注入工作记忆供 LLM 处理（感官信号作为 system 消息）
        let token_count = Self::estimate_tokens(&summary);
        self.working_memory.push_system(summary.clone(), token_count);

        // 向 Kernel 发送感官信号（让反射弧和经验学习有输入）
        if let Err(e) = self.publish_sensory_to_kernel(
            "sensory/nose/compile_error",
            &serde_json::json!({
                "error_count": error_lines.len(),
                "errors": error_lines,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送嗅探感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 视觉（Eye）：观察工具结果中的文件内容
    ///
    /// 从 Read/Glob/Grep 等工具输出中提取文件内容摘要，
    /// 并向 Kernel 发送感官信号
    async fn observe(&mut self, tool_name: &str, output: &str, transport: &NodeTransportHandle) {
        let summary = match tool_name {
            "Read" => {
                let line_count = output.lines().count();
                format!("[观察到文件内容，共 {} 行]", line_count)
            }
            "Glob" | "Grep" => {
                let match_count = output.lines().count();
                format!("[观察到 {} 条匹配结果]", match_count)
            }
            _ => return, // 非观察类工具，不处理
        };

        self.push_sensory(SensorySignal {
            source_organ: "eye".into(),
            signal_type: "file_observation".into(),
            summary,
            severity: "info".into(),
        });

        // 向 Kernel 发送感官信号
        if let Err(e) = self.publish_sensory_to_kernel(
            &format!("sensory/eye/{}", tool_name.to_lowercase()),
            &serde_json::json!({
                "tool_name": tool_name,
                "agent_id": self.id.to_string(),
            }),
            transport,
        ).await {
            tracing::debug!("发送视觉感官信号到 Kernel 失败：{}", e);
        }
    }

    /// 向 Kernel 发送感官信号
    ///
    /// ThinkerNode 内置感官处理后，通过控制面消息通知 Kernel，
    /// 让 Kernel 的 ReflexRouter 和 ExperienceLog 有输入源。
    /// topic 格式：sensory/{organ}/{detail}，如 sensory/nose/compile_error
    async fn publish_sensory_to_kernel(
        &self,
        topic: &str,
        payload: &serde_json::Value,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        let msg = FrameCodec::new_message(
            Topic::new(topic),
            self.id.as_str(),
            payload,
        )?;
        // 感官信号走控制面（经 Kernel ROUTER），确保 Kernel 一定收到
        transport.send_message(&msg).await?;
        Ok(())
    }

    /// 判断工具是否与编译/检查相关
    fn is_compile_related_tool(tool_name: &str) -> bool {
        matches!(tool_name, "Bash" | "RunCommand" | "CargoCheck" | "CargoBuild" | "CargoTest")
    }

    /// 向感官缓冲中追加信号
    fn push_sensory(&mut self, signal: SensorySignal) {
        if self.sensory_buffer.len() >= SENSORY_BUFFER_CAPACITY {
            self.sensory_buffer.remove(0);
        }
        self.sensory_buffer.push(signal);
    }

    // ── 核心推理循环 ──

    /// 构建采样请求，发送到 Sampler Node
    fn build_sample_request(&mut self) -> SampleRequest {
        let messages: Vec<ChatMessage> = self.working_memory
            .to_chat_messages()
            .into_iter()
            .map(|(role, content)| ChatMessage { role, content })
            .collect();

        // Doom Loop 逃脱：若上一轮检测到循环，本轮过滤掉被禁用的工具
        let mut tools = self.config.tools.clone();
        if let Some(tool_name) = self.disabled_tool_next_round.take() {
            tools.retain(|t| t.name != tool_name);
            tracing::warn!("Doom Loop 逃脱：本轮采样已过滤工具 {}", tool_name);
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        self.current_sample_request_id = Some(request_id.clone());

        SampleRequest {
            request_id,
            agent_id: self.id.to_string(),
            model: self.config.model.clone(),
            messages,
            tools,
            stream: true,
            reasoning_effort: self.doom_loop_detector.current_reasoning_effort(),
            max_tokens: None,
            temperature: None,
            system_prompt: None,
            tool_choice: None,
        }
    }

    /// 处理 LLM 流式返回的 chunk
    async fn handle_stream_chunk(
        &mut self,
        chunk: &StreamChunk,
        transport: &NodeTransportHandle,
    ) -> anyhow::Result<()> {
        match chunk.channel {
            StreamChannel::Text => {
                self.state = AgentState::Outputting;
                // 文本内容流式转发到 agent/{id}/output（TUINode 渲染）
                let output_msg = FrameCodec::new_message(
                    Topic::agent_output(self.id.as_str()),
                    self.id.as_str(),
                    &serde_json::json!({
                        "channel": "text",
                        "content": chunk.content,
                    }),
                )?;
                if let Err(e) = transport.publish_data(&output_msg).await {
                    tracing::debug!("数据面 PUB 发送 output 失败，回退到控制面：{}", e);
                    transport.send_message(&output_msg).await?;
                }
            }
            StreamChannel::Reasoning => {
                // 推理链内容，可选展示给用户
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
                    serde_json::from_str(&chunk.content)
                        .unwrap_or_else(|_| PendingToolCall {
                            tool_call_id: uuid::Uuid::new_v4().to_string(),
                            tool_name: "unknown".into(),
                            arguments: serde_json::Value::Null,
                        })
                };

                let args_hash = Self::hash_tool_args(&tool_call.arguments);
                self.doom_loop_detector.record(tool_call.tool_name.clone(), args_hash);

                self.pending_tool_calls.insert(tool_call.tool_call_id.clone(), tool_call);
            }
        }
        Ok(())
    }

    /// 处理工具调用结果，返回 true 表示所有工具调用已完成，应重新采样
    async fn handle_tool_result(&mut self, tool_call_id: &str, tool_name: &str, output: &str, success: bool, transport: &NodeTransportHandle) -> bool {
        self.pending_tool_results.remove(tool_call_id);

        // 内置感官处理（路线 A 核心：不经过独立器官 Node）
        // 同时向 Kernel 发送感官信号，让反射弧和经验学习有输入
        self.feel(tool_name, output, success, transport).await;   // 触觉：感知执行结果
        self.observe(tool_name, output, transport).await;          // 视觉：观察文件内容
        // sniff() 在 feel() 中自动触发（当工具失败且为编译相关工具时）

        let token_count = Self::estimate_tokens(output);
        self.short_term_memory.store(
            "tool".to_string(),
            output.to_string(),
            token_count,
            true,
        );
        // 将工具返回注入工作记忆（作为 user 消息，LLM 需要看到执行结果）
        let token_count = Self::estimate_tokens(&output);
        self.working_memory.push_user(output.to_string(), token_count);

        if self.pending_tool_results.is_empty() {
            // 通过状态机记录工具执行完成（借鉴 Claude Code）
            let outcome = if success {
                ToolExecutionOutcome::Success
            } else {
                ToolExecutionOutcome::Failure(output.chars().take(100).collect())
            };
            self.loop_state_machine.transition(LoopEvent::ToolExecutionCompleted {
                tool_use_id: tool_call_id.to_string(),
                tool_name: tool_name.to_string(),
                result: outcome,
            });
            self.state = self.loop_state_machine.state();
            true
        } else {
            false
        }
    }

    /// 每轮结束时执行滑动窗口更新
    fn update_context_window(&mut self) {
        self.turns_executed += 1;
    }

    /// 检查是否达到最大轮次
    fn check_max_turns(&self) -> bool {
        if let Some(max) = self.config.max_turns {
            self.turns_executed >= max
        } else {
            false
        }
    }

    /// 检查 Doom Loop
    fn check_doom_loop(&mut self) -> DoomLoopResult {
        self.doom_loop_detector.detect()
    }

    /// 估算文本的 token 数（粗略：1 token ≈ 4 字符）
    fn estimate_tokens(text: &str) -> u32 {
        (text.len() as f32 / 4.0).ceil() as u32
    }

    /// 计算工具参数的哈希值
    fn hash_tool_args(args: &serde_json::Value) -> u64 {
        let mut hasher = DefaultHasher::new();
        let canonical = serde_json::to_string(args).unwrap_or_else(|_| args.to_string());
        canonical.hash(&mut hasher);
        hasher.finish()
    }

    /// 发送采样请求（复用逻辑）
    async fn send_sample_request(&mut self, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        self.current_sample_request_id = None;
        AgentMetrics::global().record_loop_count(1);
        self.inference_start = Some(std::time::Instant::now());
        let request = self.build_sample_request();
        let sample_msg = FrameCodec::new_message(
            Topic::sampler_request(),
            self.id.as_str(),
            &request,
        )?;
        if let Err(e) = transport.publish_data(&sample_msg).await {
            tracing::debug!("数据面 PUB 发送采样请求失败，回退到控制面：{}", e);
            transport.send_message(&sample_msg).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl Node for ThinkerNode {
    fn node_type(&self) -> NodeType {
        NodeType::Thinker
    }

    fn node_id(&self) -> &NodeId {
        &self.id
    }

    async fn start(&mut self, _ctx: NodeContext) -> anyhow::Result<()> {
        tracing::info!(
            "Thinker Node 启动：{} (type={:?}, model={})",
            self.id, self.config.agent_type, self.config.model
        );
        self.state = AgentState::Idle;
        AgentMetrics::global().record_agent_started();
        Ok(())
    }

    async fn handle_message(&mut self, msg: Message, transport: &NodeTransportHandle) -> anyhow::Result<()> {
        let topic = msg.topic.as_str();

        match topic {
            // agent/{agent_id}/input — 收到用户输入或父 Agent 指令（兼容 AgentNode 路径）
            t if t.starts_with("agent/") && t.ends_with("/input") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let content = payload["content"].as_str().unwrap_or("");
                let role = payload["role"].as_str().unwrap_or("user");
                self.listen(content, role); // 内置听觉

                AgentMetrics::global().record_loop_count(1);
                self.inference_start = Some(std::time::Instant::now());

                let request = self.build_sample_request();
                let sample_msg = FrameCodec::new_message(
                    Topic::sampler_request(),
                    self.id.as_str(),
                    &request,
                )?;
                if let Err(e) = transport.publish_data(&sample_msg).await {
                    tracing::debug!("数据面 PUB 发送失败，回退到控制面：{}", e);
                    transport.send_message(&sample_msg).await?;
                }
            }

            // sampler/*/stream — 收到 LLM 流式返回
            t if t.starts_with("sampler/") && t.ends_with("/stream") => {
                let raw_value: serde_json::Value = FrameCodec::decode_payload(&msg)?;

                // 检查是否为 done 消息
                if raw_value.get("type").and_then(|v| v.as_str()) == Some("done") {
                    if let Some(req_id) = raw_value.get("request_id").and_then(|v| v.as_str()) {
                        if Some(req_id) != self.current_sample_request_id.as_deref() {
                            return Ok(());
                        }
                    }

                    if let Some(start) = self.inference_start.take() {
                        AgentMetrics::global()
                            .record_inference_latency(start.elapsed().as_millis() as f64);
                    }

                    if !self.pending_tool_calls.is_empty() {
                        self.state = AgentState::ToolCalling;
                        let tool_calls: Vec<(String, PendingToolCall)> =
                            self.pending_tool_calls.drain().collect();
                        for (tool_call_id, pending) in tool_calls {
                            self.pending_tool_results
                                .insert(tool_call_id.clone(), pending.tool_name.clone());

                            // 发送到 agent/{id}/tool_call（ToolNode 标准路径）
                            let tool_call_msg = FrameCodec::new_message(
                                Topic::agent_tool_call(self.id.as_str()),
                                self.id.as_str(),
                                &serde_json::json!({
                                    "tool_call_id": tool_call_id,
                                    "tool_name": pending.tool_name,
                                    "arguments": pending.arguments,
                                    "agent_id": self.id.to_string(),
                                }),
                            )?;
                            if let Err(e) = transport.publish_data(&tool_call_msg).await {
                                tracing::debug!("数据面 PUB 发送 tool_call 失败，回退到控制面：{}", e);
                                transport.send_message(&tool_call_msg).await?;
                            }

                            tracing::debug!(
                                "Thinker {} 发送工具调用：{} ({})",
                                self.id, pending.tool_name, tool_call_id
                            );
                        }
                    } else {
                        self.current_sample_request_id = None;
                        self.state = AgentState::Idle;
                        self.update_context_window();
                        tracing::debug!("Thinker {} 采样完成", self.id);
                    }
                    return Ok(());
                }

                // 检查是否为错误消息
                if raw_value.get("error").is_some() {
                    tracing::error!(
                        "Thinker {} 收到采样错误：{:?}",
                        self.id,
                        raw_value.get("error")
                    );
                    self.inference_start = None;
                    AgentMetrics::global().record_error("sampler_error");
                    self.current_sample_request_id = None;
                    self.state = AgentState::Error;
                    return Ok(());
                }

                // 正常 StreamChunk
                let chunk: StreamChunk = FrameCodec::decode_payload(&msg)?;
                if Some(&chunk.request_id) != self.current_sample_request_id.as_ref() {
                    return Ok(());
                }
                self.handle_stream_chunk(&chunk, transport).await?;
            }

            // agent/{agent_id}/tool_result — 工具执行结果（ToolNode 标准路径）
            t if t.starts_with("agent/") && t.ends_with("/tool_result") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                let tool_call_id = payload["tool_call_id"].as_str().unwrap_or("");
                let output = payload["output"].as_str().unwrap_or("");
                let success = payload["success"].as_bool().unwrap_or(true);
                let duration_ms = payload["duration_ms"].as_u64().unwrap_or(0);

                if let Some(tool_name) = self.pending_tool_results.get(tool_call_id).cloned() {
                    AgentMetrics::global()
                        .record_tool_execution_time(&tool_name, duration_ms as f64);
                    if !success {
                        AgentMetrics::global().record_error("tool_execution_failed");
                    }

                    let all_done = self.handle_tool_result(tool_call_id, &tool_name, output, success, transport).await;

                    if all_done {
                        self.send_sample_request(transport).await?;
                    }
                } else {
                    tracing::warn!("Thinker {} 收到未知 tool_call_id 的结果：{}", self.id, tool_call_id);
                }
            }

            // agent/{agent_id}/cancel — 取消请求（来自 AcpNode/TUINode）
            t if t.starts_with("agent/") && t.ends_with("/cancel") => {
                tracing::info!("Thinker {} 收到取消请求", self.id);
                self.cancel_requested = true;

                // 如果有正在进行的采样请求，转发取消到 sampler
                if let Some(ref req_id) = self.current_sample_request_id {
                    let cancel_msg = FrameCodec::new_message(
                        Topic::sampler_cancel(req_id),
                        self.id.as_str(),
                        &serde_json::json!({
                            "request_id": req_id,
                            "agent_id": self.id.to_string(),
                        }),
                    )?;
                    if let Err(e) = transport.publish_data(&cancel_msg).await {
                        tracing::debug!("数据面 PUB 发送采样取消失败，回退到控制面：{}", e);
                        transport.send_message(&cancel_msg).await?;
                    }
                    tracing::info!("Thinker {} 已转发取消到 sampler: {}", self.id, req_id);
                }

                // 清理待处理的工具调用
                self.pending_tool_calls.clear();
                self.pending_tool_results.clear();
                self.current_sample_request_id = None;
                self.state = AgentState::Idle;
            }

            // tool/register — 收到工具注册消息
            "tool/register" => {
                let tools: Vec<SamplerToolDefinition> = FrameCodec::decode_payload(&msg)?;
                self.config.tools = tools;
                tracing::info!("Thinker {} 收到工具注册，共 {} 个工具", self.id, self.config.tools.len());
            }

            // sys/shutdown — 收到系统 shutdown 信号
            "sys/shutdown" => {
                self.state = AgentState::Done;
            }

            // 收到子 Agent 事件
            t if t.ends_with("/event") => {
                let payload: serde_json::Value = FrameCodec::decode_payload(&msg)?;
                if let Some(error) = payload.get("error") {
                    let crashed = SubAgentCrashed {
                        node_id: payload["node_id"].as_str().unwrap_or("").into(),
                        error: error.as_str().unwrap_or("unknown").into(),
                    };
                    tracing::warn!("子 Agent 崩溃：{} - {}", crashed.node_id, crashed.error);
                    AgentMetrics::global().record_error("subagent_crashed");
                    let node_id: NodeId = crashed.node_id.clone().into();
                    self.orchestrator.remove_subagent(&node_id);
                } else if payload.get("type").and_then(|v| v.as_str()) == Some("completed") {
                    let subagent_id = payload["node_id"].as_str().unwrap_or("");
                    let output = payload["output"].as_str().unwrap_or("");
                    tracing::info!("子 Agent 完成：{}, 输出长度={}", subagent_id, output.len());
                    let token_count = Self::estimate_tokens(output);
                    self.working_memory.push_user(output.to_string(), token_count);
                    let node_id: NodeId = subagent_id.to_string().into();
                    self.orchestrator.remove_subagent(&node_id);
                }
            }

            _ => {
                tracing::debug!("Thinker 收到未处理 topic：{}", topic);
            }
        }

        // 检查 Doom Loop 并应用逃脱策略
        let doom_result = self.check_doom_loop();
        if doom_result.detected {
            tracing::warn!(
                "Doom Loop 检测触发：工具 {:?} 重复 {} 次，应用逃脱策略（{} 个动作）",
                doom_result.repeated_tool, doom_result.repeat_count, doom_result.escape_actions.len()
            );
            AgentMetrics::global().record_error("doom_loop");

            // 通过 LoopStateMachine 处理 Doom Loop 事件（借鉴 Claude Code 状态机驱动）
            if let Some(loop_event) = LoopEvent::from_doom_loop(&doom_result) {
                let action = self.loop_state_machine.transition(loop_event);
                self.state = self.loop_state_machine.state();
                tracing::info!(
                    "LoopStateMachine 状态变迁：action={:?}, state={:?}",
                    action, self.state
                );
                // 如果状态机判定结束，通知用户并结束
                if let LoopAction::EndTurn { reason } = action {
                    tracing::warn!("LoopStateMachine 判定结束：reason={:?}", reason);
                    let notice_msg = FrameCodec::new_message(
                        Topic::agent_output(self.id.as_str()),
                        self.id.as_str(),
                        &serde_json::json!({
                            "channel": "text",
                            "content": format!("Agent 循环终止：{:?}", reason),
                        }),
                    )?;
                    if let Err(e) = transport.publish_data(&notice_msg).await {
                        tracing::debug!("数据面 PUB 发送循环终止通知失败：{}", e);
                        transport.send_message(&notice_msg).await?;
                    }
                    return Ok(());
                }
            }

            // 如果状态机没有判定结束（Doom Loop 逃脱策略生效），继续执行逃脱动作
            for action in &doom_result.escape_actions {
                match action {
                    EscapeAction::InjectHint(hint) => {
                        self.working_memory.push_system(hint.clone(), Self::estimate_tokens(hint));
                        tracing::warn!("Doom Loop 逃脱：已注入换策略提示到工作记忆");
                    }
                    EscapeAction::DisableTool(tool) => {
                        self.disabled_tool_next_round = Some(tool.clone());
                        tracing::warn!("Doom Loop 逃脱：下一轮禁用工具 {}", tool);
                    }
                    EscapeAction::DegradeModel => {
                        let level = self.doom_loop_detector.model_degrade_level();
                        tracing::warn!(
                            "Doom Loop 逃脱：降级模型（reasoning_effort level={}）",
                            level
                        );
                    }
                }
            }

            // 通知用户 Doom Loop 逃脱正在尝试
            let notice_msg = FrameCodec::new_message(
                Topic::agent_output(self.id.as_str()),
                self.id.as_str(),
                &serde_json::json!({
                    "channel": "text",
                    "content": format!(
                        "检测到工具循环（{} 重复 {} 次），已注入换策略提示、临时禁用该工具并降级模型，正在尝试跳出循环。",
                        doom_result.repeated_tool.as_deref().unwrap_or("unknown"),
                        doom_result.repeat_count
                    ),
                }),
            )?;
            if let Err(e) = transport.publish_data(&notice_msg).await {
                tracing::debug!("数据面 PUB 发送 doom loop 通知失败，回退到控制面：{}", e);
                transport.send_message(&notice_msg).await?;
            }
        }

        // 检查是否需要自动压缩（借鉴 Claude Code CompactionPolicy）
        if self.working_memory.should_compact() {
            tracing::info!(
                "工作记忆使用率达压缩阈值，执行自动压缩（tokens_used={}/max={})",
                self.working_memory.used_tokens(),
                self.working_memory.max_tokens()
            );
            let result = self.working_memory.compact();
            tracing::info!(
                "工作记忆压缩完成：before={}, after={}, compacted_entries={}",
                result.tokens_before, result.tokens_after, result.entries_compacted
            );
        }

        // 检查最大轮次（通过状态机）
        if self.check_max_turns() {
            tracing::info!("达到最大轮次 {}，Thinker 结束", self.turns_executed);
            let action = self.loop_state_machine.transition(LoopEvent::MaxTurnsReached);
            self.state = self.loop_state_machine.state();
            tracing::debug!("LoopStateMachine MaxTurnsReached：action={:?}", action);
        }

        Ok(())
    }

    fn subscriptions(&self) -> Vec<String> {
        let mut subs = vec![
            format!("agent/{}/input", self.id),       // 用户输入（替代 cortex/{id}/input）
            format!("agent/{}/tool_result", self.id),  // 工具结果（替代 skin/touch）
            format!("agent/{}/cancel", self.id),       // 取消请求（来自 AcpNode/TUINode）
            "sampler/*/stream".into(),
            "tool/register".into(),
            "sys/shutdown".into(),
        ];

        // 如果有活跃子 Agent，也订阅它们的输出和事件
        for sub in self.orchestrator.active_subagents() {
            subs.push(format!("agent/{}/output", sub.node_id));
            subs.push(format!("agent/{}/event", sub.node_id));
        }

        subs
    }

    fn published_topics(&self) -> Vec<String> {
        vec![
            format!("agent/{}/output", self.id),       // 文本输出（替代 cortex/{id}/speak）
            format!("agent/{}/tool_call", self.id),     // 工具调用（替代 hand/limb/*）
        ]
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.state = AgentState::Done;
        tracing::info!("Thinker Node 关闭：{}", self.id);
        AgentMetrics::global().record_agent_stopped();
        Ok(())
    }
}
