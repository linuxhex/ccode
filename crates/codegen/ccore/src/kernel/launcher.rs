//! Node 启动器 - 统一管理 Node 进程的 spawn、连接、注册
//!
//! NodeLauncher 负责：
//! 1. 根据 KernelConfig spawn 初始 Node 集合
//! 2. 每个 Node 在独立 tokio task 中通过 run_node() 启动
//! 3. 管理 Node 进程的生命周期（启动、停止、重启）
//! 4. 监控 Node 进程健康状态
//!
//! 启动流程：
//! Kernel 启动 → NodeLauncher spawn 各 Node → 各 Node 连接 ZMQ → 发送 sys/register → 开始工作
//!
//! 仿生架构（路线 A）+ ACP 融合：6 个 Node
//! - Sampler：LLM 采样
//! - State：对话持久化
//! - Tool：工具执行（= Hand + Limb 运动层）
//! - Thinker：大脑皮层（内置 Eye/Ear/Nose/Skin 感官层）
//! - TUI：用户交互（= Ear + Mouth 交互层）
//! - Acp：IDE/stdio ACP client 与消息总线边界

use std::sync::Arc;

use crate::kernel::{KernelConfig, KernelRuntimeConfig};
use crate::node::{
    NodeId, NodeType, NodeContext,
};
use crate::node::sampler::SamplerNode;
use crate::node::tui::TUINode;
use crate::node::tool::ToolNode;
use crate::node::state::StateNode;
use crate::node::thinker::{ThinkerNode, EpisodicMemoryBridge};
use crate::node::acp::AcpNode;
use crate::node::transport::run_node;
use crate::agent::AgentConfig;
use crate::agent::AgentType;
use crate::agent::subagent::{SubAgentNode, SubAgentDefinition};
use crate::tools::hook_bridge::HookDispatcher;

/// Node 进程描述
#[derive(Debug, Clone)]
pub struct NodeDescriptor {
    pub id: NodeId,
    pub node_type: NodeType,
    pub name: String,
}

/// Node 启动器
pub struct NodeLauncher {
    /// Kernel 配置
    kernel_config: KernelConfig,
    /// 运行时配置（providers/model/permission）
    runtime_config: KernelRuntimeConfig,
    /// 情景记忆存储（与 Kernel 共享同一实例）
    episodic_memory: std::sync::Arc<crate::memory::episodic::EpisodicMemoryStore>,
    /// HookDispatcher 桥接（可选，由产品层注入 ccode-hooks 适配器）
    hook_dispatcher: Option<Arc<dyn HookDispatcher>>,
    /// 已启动的 Node 描述
    launched_nodes: Vec<NodeDescriptor>,
    /// 已启动的 Node 任务 JoinHandle（用于优雅关闭）
    task_handles: Vec<tokio::task::JoinHandle<()>>,
}

impl NodeLauncher {
    pub fn new(
        kernel_config: KernelConfig,
        runtime_config: KernelRuntimeConfig,
        episodic_memory: std::sync::Arc<crate::memory::episodic::EpisodicMemoryStore>,
    ) -> Self {
        Self {
            kernel_config,
            runtime_config,
            episodic_memory,
            hook_dispatcher: None,
            launched_nodes: Vec::new(),
            task_handles: Vec::new(),
        }
    }

    /// 设置 HookDispatcher（由产品层注入 ccode-hooks 适配器）
    pub fn set_hook_dispatcher(&mut self, dispatcher: Arc<dyn HookDispatcher>) {
        self.hook_dispatcher = Some(dispatcher);
    }

    /// 获取 NodeContext（供各 Node 连接消息总线）
    fn node_context(&self) -> NodeContext {
        let node_id = NodeId::new();
        NodeContext {
            router_addr: self.kernel_config.router_addr.clone(),
            pub_addr: self.kernel_config.pub_addr.clone(),
            data_pub_addr: format!("ipc:///tmp/ccode-pub-{}", node_id),
            data_rep_addr: None,
        }
    }

    /// spawn 初始 Node 集合（仿生架构路线 A + ACP 融合：6 Node）
    ///
    /// 每个 Node 在独立的 tokio task 中通过 run_node() 启动，
    /// run_node() 会自动连接消息总线、注册、进入消息循环。
    ///
    /// 架构（仿生隐喻保留在概念层面，不拆独立进程）：
    /// 1. Sampler（LLM 采样）— Thinker 依赖
    /// 2. State（对话持久化）— Thinker 依赖
    /// 3. Tool（工具执行 = Hand + Limb 运动层）— Thinker 依赖
    /// 4. Thinker（大脑皮层 + 内置 Eye/Ear/Nose/Skin 感官层）— 核心
    /// 5. TUI（用户交互 = Ear + Mouth 交互层）— 用户界面
    /// 6. Acp（IDE/stdio ACP client 边界）— 外部协议适配
    pub async fn spawn_initial_set(&mut self) -> anyhow::Result<Vec<NodeDescriptor>> {
        // 并行 spawn 6 个 Node（构造 + tokio::spawn 均在并行块中完成）
        // tokio::join! 确保所有 spawn 完成后再继续

        // 1. Sampler Node
        let sampler_ctx = self.node_context();
        let sampler_id = NodeId::new();
        let sampler_id_clone = sampler_id.clone();
        let sampler = SamplerNode::with_configs(sampler_id.clone(), &self.runtime_config.providers);

        // 2. State Node
        let state_ctx = self.node_context();
        let state_id = NodeId::new();
        let state_id_clone = state_id.clone();
        let state = StateNode::new(state_id.clone());

        // 3. Tool Node
        let tool_ctx = self.node_context();
        let tool_id = NodeId::new();
        let tool_id_clone = tool_id.clone();
        let mut tool = ToolNode::new(tool_id.clone());

        // 注入 HookDispatcher 桥接（如果产品层已设置）
        if let Some(dispatcher) = self.hook_dispatcher.take() {
            tool.set_hook_dispatcher(dispatcher);
        }

        // 4. Thinker Node
        let thinker_ctx = self.node_context();
        let thinker_id = NodeId::new();
        let thinker_id_clone = thinker_id.clone();
        let thinker_config = AgentConfig {
            agent_type: AgentType::Primary,
            model: self.runtime_config.default_model.clone(),
            permission_mode: self.runtime_config.permission_mode,
            max_turns: None,
            subagents_enabled: true,
            non_interactive: false,
            tools: Vec::new(),
        };
        let mut thinker = ThinkerNode::new(thinker_id.clone(), thinker_config);

        // 连接情景记忆桥接（ThinkerNode ↔ EpisodicMemoryStore，与 Kernel 共享同一实例）
        thinker.set_memory_bridge(Box::new(EpisodicMemoryBridge::new(self.episodic_memory.clone())));

        // 5. TUI Node
        let tui_ctx = self.node_context();
        let tui_id = NodeId::new();
        let tui_id_clone = tui_id.clone();
        let tui = TUINode::new(tui_id.clone());

        // 6. Acp Node — 与 Thinker 共享同一 primary_agent_id
        let primary_agent_id = thinker_id.as_str().to_string();
        let acp_ctx = self.node_context();
        let acp_id = NodeId::new();
        let acp_id_clone = acp_id.clone();
        let acp = AcpNode::new(acp_id.clone(), primary_agent_id);

        // 并行 spawn 所有 Node（收集 JoinHandle，不立即 await）
        let sampler_handle = tokio::spawn(async move {
            if let Err(e) = run_node(sampler, sampler_ctx).await {
                tracing::error!("Sampler Node 异常退出：{}", e);
            }
        });
        let state_handle = tokio::spawn(async move {
            if let Err(e) = run_node(state, state_ctx).await {
                tracing::error!("State Node 异常退出：{}", e);
            }
        });
        let tool_handle = tokio::spawn(async move {
            if let Err(e) = run_node(tool, tool_ctx).await {
                tracing::error!("Tool Node 异常退出：{}", e);
            }
        });
        let thinker_handle = tokio::spawn(async move {
            if let Err(e) = run_node(thinker, thinker_ctx).await {
                tracing::error!("Thinker Node 异常退出：{}", e);
            }
        });
        let tui_handle = tokio::spawn(async move {
            if let Err(e) = crate::node::tui::run_tui_node(tui, tui_ctx).await {
                tracing::error!("TUI Node 异常退出：{}", e);
            }
        });
        let acp_handle = tokio::spawn(async move {
            if let Err(e) = run_node(acp, acp_ctx).await {
                tracing::error!("Acp Node 异常退出：{}", e);
            }
        });

        // 收集 handles 和描述符
        self.task_handles.push(sampler_handle);
        self.task_handles.push(state_handle);
        self.task_handles.push(tool_handle);
        self.task_handles.push(thinker_handle);
        self.task_handles.push(tui_handle);
        self.task_handles.push(acp_handle);

        self.launched_nodes.push(NodeDescriptor {
            id: sampler_id_clone,
            node_type: NodeType::Sampler,
            name: "sampler-primary".into(),
        });
        self.launched_nodes.push(NodeDescriptor {
            id: state_id_clone,
            node_type: NodeType::State,
            name: "state-primary".into(),
        });
        self.launched_nodes.push(NodeDescriptor {
            id: tool_id_clone,
            node_type: NodeType::Tool,
            name: "tool-primary".into(),
        });
        self.launched_nodes.push(NodeDescriptor {
            id: thinker_id_clone,
            node_type: NodeType::Thinker,
            name: "thinker-primary".into(),
        });
        self.launched_nodes.push(NodeDescriptor {
            id: tui_id_clone,
            node_type: NodeType::TUI,
            name: "tui-primary".into(),
        });
        self.launched_nodes.push(NodeDescriptor {
            id: acp_id_clone,
            node_type: NodeType::Acp,
            name: "acp-primary".into(),
        });

        tracing::info!("仿生架构（路线 A）+ ACP 融合初始 Node 集合启动完成（共 {} 个）", self.launched_nodes.len());
        Ok(self.launched_nodes.clone())
    }

    /// spawn 子 Agent（使用 SubAgentNode 获得工具隔离 + 资源限制）
    pub fn spawn_subagent(
        &mut self,
        agent_type: AgentType,
        model: Option<String>,
        task_description: String,
    ) -> NodeDescriptor {
        let ctx = self.node_context();
        let id = NodeId::new();
        let parent_id = self.launched_nodes
            .iter()
            .find(|n| n.node_type == NodeType::Thinker)
            .map(|n| n.id.clone())
            .unwrap_or_else(NodeId::new);
        let agent_config = AgentConfig {
            agent_type,
            model: model.unwrap_or_else(|| self.runtime_config.default_model.clone()),
            permission_mode: self.runtime_config.permission_mode,
            max_turns: Some(20),
            subagents_enabled: false,
            non_interactive: true,
            tools: Vec::new(),
        };
        let definition = SubAgentDefinition {
            agent_type,
            model: None,
            task_description: task_description.clone(),
            max_turns: 20,
            allowed_tools: Vec::new(),
        };
        let agent = SubAgentNode::new(id.clone(), parent_id, agent_config, definition);
        let sub_id = id.clone();
        let agent_handle = tokio::spawn(async move {
            if let Err(e) = run_node(agent, ctx).await {
                tracing::error!("子 Agent {} 异常退出：{}", sub_id, e);
            }
        });
        self.task_handles.push(agent_handle);

        let descriptor = NodeDescriptor {
            id: id.clone(),
            node_type: NodeType::Agent,
            name: format!("agent-sub-{}", id),
        };
        self.launched_nodes.push(descriptor.clone());
        tracing::info!("子 Agent 已 spawn：{} ({:?})", id, agent_type);
        descriptor
    }

    /// 获取已启动的 Node 列表
    pub fn launched_nodes(&self) -> &[NodeDescriptor] {
        &self.launched_nodes
    }

    /// 按 ID 查找 Node
    pub fn find_node(&self, id: &NodeId) -> Option<&NodeDescriptor> {
        self.launched_nodes.iter().find(|n| n.id == *id)
    }

    /// 按类型查找 Node
    pub fn find_by_type(&self, node_type: NodeType) -> Vec<&NodeDescriptor> {
        self.launched_nodes
            .iter()
            .filter(|n| n.node_type == node_type)
            .collect()
    }

    /// 优雅关闭所有已启动的 Node 任务
    ///
    /// 1. 向所有任务发送 abort 信号
    /// 2. 等待所有任务退出（最多 5 秒超时）
    /// 3. 超时后记录警告但仍然返回（任务会在进程退出时被清理）
    pub async fn graceful_shutdown(&mut self) {
        let handles = std::mem::take(&mut self.task_handles);
        if handles.is_empty() {
            return;
        }

        tracing::info!(
            target: "ccore::kernel",
            count = handles.len(),
            "graceful shutdown: aborting {} node tasks",
            handles.len()
        );

        // 1. 请求所有任务取消
        for handle in &handles {
            handle.abort();
        }

        // 2. 等待所有任务退出，带超时
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);

        for handle in handles {
            tokio::select! {
                _ = handle => {},
                _ = &mut deadline => {
                    tracing::warn!(
                        target: "ccore::kernel",
                        "graceful shutdown timeout, some node tasks didn't finish"
                    );
                    break;
                }
            }
        }

        tracing::info!(target: "ccore::kernel", "node tasks shutdown complete");
    }

    /// 获取已启动任务的数量
    pub fn task_count(&self) -> usize {
        self.task_handles.len()
    }
}

impl Drop for NodeLauncher {
    fn drop(&mut self) {
        if !self.task_handles.is_empty() {
            tracing::debug!(
                target: "ccore::kernel",
                count = self.task_handles.len(),
                "dropping NodeLauncher, aborting orphaned node tasks"
            );
            for handle in &self.task_handles {
                handle.abort();
            }
        }
    }
}
