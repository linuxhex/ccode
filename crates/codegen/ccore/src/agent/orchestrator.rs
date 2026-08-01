//! Agent 编排器 - 管理主 Agent 和子 Agent 的交互
//!
//! 借鉴 Claude Code 的 Coordinator-Worker 模式：
//! - 并行 spawn：多个子 Agent 同时执行独立任务
//! - Atomic Claim：防止多个 Worker 重复处理同一任务
//! - Team Memory：子 Agent 间共享上下文

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use crate::agent::subagent::{SubAgentState, SubAgentDefinition};
use crate::node::NodeId;

/// 任务认领记录（Atomic Claim）
#[derive(Debug, Clone)]
pub struct TaskClaim {
    /// 任务标识（如文件路径、函数名）
    pub task_id: String,
    /// 认领该任务的子 Agent ID
    pub claimant: NodeId,
    /// 认领时间
    pub claimed_at: std::time::Instant,
}

/// Team Memory 条目
#[derive(Debug, Clone)]
pub struct TeamMemoryEntry {
    /// 发布者（子 Agent ID）
    pub source: NodeId,
    /// 内容
    pub content: String,
    /// 时间戳
    pub timestamp: std::time::Instant,
}

/// Agent 编排器
pub struct Orchestrator {
    /// 活跃的子 Agent
    subagents: HashMap<NodeId, SubAgentState>,
    /// 最大子 Agent 数量
    max_subagents: usize,
    /// 任务认领表（Atomic Claim：防止多个 Worker 重复认领同一任务）
    claims: HashMap<String, TaskClaim>,
    /// Team Memory（子 Agent 间共享上下文）
    team_memory: Vec<TeamMemoryEntry>,
    /// Team Memory 最大条目数
    max_team_memory: usize,
}

impl Orchestrator {
    pub fn new(max_subagents: usize) -> Self {
        Self {
            subagents: HashMap::new(),
            max_subagents,
            claims: HashMap::new(),
            team_memory: Vec::new(),
            max_team_memory: 100,
        }
    }

    /// 是否可以创建新的子 Agent
    pub fn can_spawn(&self) -> bool {
        self.subagents.len() < self.max_subagents
    }

    /// 注册子 Agent
    pub fn register_subagent(&mut self, node_id: NodeId, definition: SubAgentDefinition) {
        let state = SubAgentState {
            node_id: node_id.clone(),
            definition,
            state: crate::agent::AgentState::Thinking,
            output: None,
        };
        self.subagents.insert(node_id, state);
    }

    /// 更新子 Agent 状态
    pub fn update_state(&mut self, node_id: &NodeId, state: crate::agent::AgentState) {
        if let Some(sub) = self.subagents.get_mut(node_id) {
            sub.state = state;
        }
    }

    /// 设置子 Agent 输出
    pub fn set_output(&mut self, node_id: &NodeId, output: String) {
        if let Some(sub) = self.subagents.get_mut(node_id) {
            sub.output = Some(output);
            sub.state = crate::agent::AgentState::Done;
        }
    }

    /// 移除已完成的子 Agent
    pub fn remove_completed(&mut self, node_id: &NodeId) {
        self.subagents.remove(node_id);
    }

    /// 移除子 Agent（无论完成或崩溃）
    pub fn remove_subagent(&mut self, node_id: &NodeId) {
        if self.subagents.remove(node_id).is_some() {
            tracing::debug!("子 Agent 已从编排器移除：{}", node_id);
            // 释放该子 Agent 的所有认领
            self.claims.retain(|_, c| &c.claimant != node_id);
        }
    }

    /// 获取所有活跃子 Agent
    pub fn active_subagents(&self) -> Vec<&SubAgentState> {
        self.subagents.values().collect()
    }

    /// 检查所有子 Agent 是否都已完成
    pub fn all_completed(&self) -> bool {
        self.subagents.values().all(|s| s.state == crate::agent::AgentState::Done)
    }

    // ─── Atomic Claim ──────────────────────────────────────────────────────

    /// 原子认领任务：返回 true 表示认领成功，false 表示已被其他 Agent 认领
    pub fn claim_task(&mut self, task_id: &str, claimant: &NodeId) -> bool {
        if self.claims.contains_key(task_id) {
            return false;
        }
        self.claims.insert(task_id.to_string(), TaskClaim {
            task_id: task_id.to_string(),
            claimant: claimant.clone(),
            claimed_at: std::time::Instant::now(),
        });
        true
    }

    /// 释放任务认领（子 Agent 完成或失败后调用）
    pub fn release_claim(&mut self, task_id: &str) {
        self.claims.remove(task_id);
    }

    /// 检查任务是否已被认领
    pub fn is_claimed(&self, task_id: &str) -> bool {
        self.claims.contains_key(task_id)
    }

    /// 获取当前所有认领
    pub fn active_claims(&self) -> &HashMap<String, TaskClaim> {
        &self.claims
    }

    // ─── Team Memory ───────────────────────────────────────────────────────

    /// 发布共享记忆条目（子 Agent 向 Team Memory 写入发现/进展）
    pub fn publish_team_memory(&mut self, source: &NodeId, content: String) {
        self.team_memory.push(TeamMemoryEntry {
            source: source.clone(),
            content,
            timestamp: std::time::Instant::now(),
        });
        // 超过上限时淘汰最旧的条目
        if self.team_memory.len() > self.max_team_memory {
            self.team_memory.drain(0..self.team_memory.len() - self.max_team_memory);
        }
    }

    /// 获取 Team Memory 中所有条目（供子 Agent 读取共享上下文）
    pub fn team_memory_entries(&self) -> &[TeamMemoryEntry] {
        &self.team_memory
    }

    /// 获取 Team Memory 的文本摘要（可注入到子 Agent 的 system prompt）
    pub fn team_memory_summary(&self) -> String {
        if self.team_memory.is_empty() {
            return String::new();
        }
        let mut summary = String::from("[Team Memory] 协作进展：\n");
        for entry in self.team_memory.iter().rev().take(10) {
            summary.push_str(&format!("- {:?}: {}\n", entry.source, entry.content));
        }
        summary
    }

    /// 清空 Team Memory（新一轮任务开始时调用）
    pub fn clear_team_memory(&mut self) {
        self.team_memory.clear();
    }

    // ─── 并行编排 ──────────────────────────────────────────────────────────

    /// 并行 spawn 多个子 Agent 执行独立任务
    ///
    /// 返回成功 spawn 的子 Agent ID 列表。跳过已认领的任务。
    pub fn spawn_parallel(
        &mut self,
        tasks: Vec<(String, SubAgentDefinition)>,
    ) -> Vec<(NodeId, String)> {
        let mut spawned = Vec::new();
        for (task_id, definition) in tasks {
            if !self.can_spawn() {
                tracing::warn!("并行 spawn 达到上限 ({}), 跳过剩余任务", self.max_subagents);
                break;
            }
            if self.is_claimed(&task_id) {
                tracing::debug!("任务 {} 已被认领，跳过", task_id);
                continue;
            }
            let node_id = NodeId::new();
            self.claim_task(&task_id, &node_id);
            self.register_subagent(node_id.clone(), definition);
            spawned.push((node_id, task_id));
        }
        spawned
    }

    /// 收集所有已完成子 Agent 的结果
    pub fn collect_results(&mut self) -> Vec<(NodeId, String)> {
        let mut results = Vec::new();
        let completed_ids: Vec<NodeId> = self.subagents
            .iter()
            .filter(|(_, s)| s.state == crate::agent::AgentState::Done)
            .map(|(id, _)| id.clone())
            .collect();

        for id in completed_ids {
            if let Some(sub) = self.subagents.get(&id) {
                if let Some(output) = &sub.output {
                    results.push((id.clone(), output.clone()));
                }
            }
        }
        results
    }
}

/// 线程安全的 Orchestrator 包装（用于跨 task 共享）
pub struct SharedOrchestrator {
    inner: Arc<Mutex<Orchestrator>>,
}

impl SharedOrchestrator {
    pub fn new(max_subagents: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Orchestrator::new(max_subagents))),
        }
    }

    pub fn with_lock<R>(&self, f: impl FnOnce(&mut Orchestrator) -> R) -> R {
        f(&mut self.inner.lock().expect("orchestrator lock"))
    }

    pub fn with_read<R>(&self, f: impl FnOnce(&Orchestrator) -> R) -> R {
        f(&self.inner.lock().expect("orchestrator lock"))
    }
}
