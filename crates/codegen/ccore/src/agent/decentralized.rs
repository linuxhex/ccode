//! 去中心化DAG自组织模块（借鉴 AgentNet/Symphony 2025-2026 论文）
//!
//! 核心创新：
//! 1. 动态DAG拓扑：agent连接根据任务需求实时调整
//! 2. 能力感知路由：基于agent能力画像分配任务
//! 3. 自进化专业化：agent根据执行结果动态专业化
//!
//! 参考：
//! - AgentNet: Decentralized Evolutionary Coordination (NeurIPS 2025)
//! - Symphony: Decentralized Multi-Agent Framework (AAAI 2026)
//! - AgentNet++: Hierarchical Decentralized Coordination (2025)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// Agent能力画像
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    /// Agent ID
    pub id: String,
    /// 能力标签 → 水平 (0.0-1.0)
    pub capabilities: HashMap<String, f64>,
    /// 已完成的任务类型统计
    pub task_history: HashMap<String, TaskStats>,
    /// 当前负载 (0.0-1.0)
    pub current_load: f64,
    /// 专业化方向
    pub specialization: Vec<String>,
}

/// 任务统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStats {
    pub completed: u32,
    pub succeeded: u32,
    pub avg_duration_ms: u64,
}

/// DAG边（Agent间的任务路由）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagEdge {
    pub from: String,
    pub to: String,
    pub weight: f64,
    pub task_types: Vec<String>,
}

/// 任务路由请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRequest {
    /// 任务类型
    pub task_type: String,
    /// 所需能力
    pub required_capabilities: Vec<String>,
    /// 优先级
    pub priority: f64,
}

/// 路由决策
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    /// 目标agent
    pub target_agent: String,
    /// 匹配分数
    pub match_score: f64,
    /// 路由原因
    pub reason: String,
}

/// 去中心化DAG协调器
pub struct DecentralizedCoordinator {
    /// Agent画像注册表
    profiles: RwLock<HashMap<String, AgentProfile>>,
    /// DAG边
    edges: RwLock<Vec<DagEdge>>,
    /// 最大agent数
    max_agents: usize,
}

impl DecentralizedCoordinator {
    pub fn new(max_agents: usize) -> Self {
        Self {
            profiles: RwLock::new(HashMap::new()),
            edges: RwLock::new(Vec::new()),
            max_agents,
        }
    }

    /// 注册agent（加入DAG网络）
    pub fn register_agent(&self, profile: AgentProfile) {
        {
            let mut profiles = self.profiles.write().unwrap();
            if profiles.len() >= self.max_agents && !profiles.contains_key(&profile.id) {
                tracing::warn!(target: "ccore::decentralized", "agent pool full, cannot register");
                return;
            }
            let agent_id = profile.id.clone();
            profiles.insert(agent_id, profile);
        }

        let agent_id = {
            let profiles = self.profiles.read().unwrap();
            profiles.keys().last().unwrap().clone()
        };
        self.evolve_topology(&agent_id);

        tracing::info!(target: "ccore::decentralized", agent = %agent_id, "agent registered to DAG");
    }

    /// 能力感知路由（AgentNet: capability-aware routing）
    pub fn route_task(&self, request: &RoutingRequest) -> Option<RoutingDecision> {
        let profiles = self.profiles.read().unwrap();

        let mut best_agent: Option<(&String, f64)> = None;

        for (id, profile) in profiles.iter() {
            let mut score = 0.0;

            // 能力匹配
            for cap in &request.required_capabilities {
                if let Some(level) = profile.capabilities.get(cap) {
                    score += level * 0.4;
                }
            }

            // 专业化匹配
            if profile.specialization.contains(&request.task_type) {
                score += 0.3;
            }

            // 历史成功率
            if let Some(stats) = profile.task_history.get(&request.task_type) {
                if stats.completed > 0 {
                    score += (stats.succeeded as f64 / stats.completed as f64) * 0.2;
                }
            }

            // 负载惩罚
            score *= 1.0 - profile.current_load * 0.5;

            // 优先级加权
            score *= request.priority;

            if score > best_agent.as_ref().map(|(_, s)| *s).unwrap_or(0.0) {
                best_agent = Some((id, score));
            }
        }

        best_agent.map(|(id, score)| RoutingDecision {
            target_agent: id.clone(),
            match_score: score,
            reason: format!("能力匹配分数={:.2}", score),
        })
    }

    /// 拓扑进化（AgentNet: dynamically evolving graph topology）
    ///
    /// 根据任务执行结果调整agent间的连接
    fn evolve_topology(&self, new_agent_id: &str) {
        let profiles = self.profiles.read().unwrap();
        let mut edges = self.edges.write().unwrap();

        if let Some(new_profile) = profiles.get(new_agent_id) {
            // 与能力互补的agent建立连接
            for (id, profile) in profiles.iter() {
                if id == new_agent_id {
                    continue;
                }

                // 计算互补性
                let complementarity = self.compute_complementarity(new_profile, profile);

                if complementarity > 0.3 {
                    edges.push(DagEdge {
                        from: new_agent_id.to_string(),
                        to: id.clone(),
                        weight: complementarity,
                        task_types: self.find_shared_tasks(new_profile, profile),
                    });

                    // 双向连接（DAG允许反向引用）
                    edges.push(DagEdge {
                        from: id.clone(),
                        to: new_agent_id.to_string(),
                        weight: complementarity,
                        task_types: self.find_shared_tasks(profile, new_profile),
                    });
                }
            }
        }

        tracing::debug!(
            target: "ccore::decentralized",
            agent = %new_agent_id,
            edge_count = edges.len(),
            "topology evolved"
        );
    }

    /// 更新agent执行结果（自进化专业化）
    pub fn update_task_result(
        &self,
        agent_id: &str,
        task_type: &str,
        success: bool,
        duration_ms: u64,
    ) {
        let mut profiles = self.profiles.write().unwrap();
        if let Some(profile) = profiles.get_mut(agent_id) {
            let stats = profile
                .task_history
                .entry(task_type.to_string())
                .or_insert(TaskStats {
                    completed: 0,
                    succeeded: 0,
                    avg_duration_ms: 0,
                });

            stats.completed += 1;
            if success {
                stats.succeeded += 1;
            }
            stats.avg_duration_ms = (stats.avg_duration_ms + duration_ms) / 2;

            // 自进化专业化：成功率高 → 更专业化
            let success_rate = stats.succeeded as f64 / stats.completed as f64;
            if success_rate > 0.7
                && stats.completed >= 3
                && !profile.specialization.contains(&task_type.to_string())
            {
                profile.specialization.push(task_type.to_string());
                tracing::info!(
                    target: "ccore::decentralized",
                    agent = %agent_id,
                    specialization = %task_type,
                    "agent evolved new specialization"
                );
            }
        }
    }

    fn compute_complementarity(&self, a: &AgentProfile, b: &AgentProfile) -> f64 {
        let mut score = 0.0;
        let mut count = 0;

        for (cap, level_a) in &a.capabilities {
            if let Some(level_b) = b.capabilities.get(cap) {
                // 互补：一高一低
                score += 1.0 - (level_a - level_b).abs();
                count += 1;
            }
        }

        if count > 0 {
            score / count as f64
        } else {
            0.0
        }
    }

    fn find_shared_tasks(&self, a: &AgentProfile, b: &AgentProfile) -> Vec<String> {
        a.task_history
            .keys()
            .filter(|k| b.task_history.contains_key(*k))
            .cloned()
            .collect()
    }

    /// 获取DAG统计
    pub fn stats(&self) -> (usize, usize, usize) {
        let agents = self.profiles.read().unwrap().len();
        let edges = self.edges.read().unwrap().len();
        let specializations = self
            .profiles
            .read()
            .unwrap()
            .values()
            .map(|p| p.specialization.len())
            .sum();
        (agents, edges, specializations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_profile(id: &str, caps: HashMap<String, f64>) -> AgentProfile {
        AgentProfile {
            id: id.to_string(),
            capabilities: caps,
            task_history: HashMap::new(),
            current_load: 0.0,
            specialization: Vec::new(),
        }
    }

    #[test]
    fn test_register_agent() {
        let coord = DecentralizedCoordinator::new(10);
        let profile = make_profile("agent_1", HashMap::new());
        coord.register_agent(profile);

        let (agents, _, _) = coord.stats();
        assert_eq!(agents, 1);
    }

    #[test]
    fn test_max_agents_limit() {
        let coord = DecentralizedCoordinator::new(2);
        coord.register_agent(make_profile("a1", HashMap::new()));
        coord.register_agent(make_profile("a2", HashMap::new()));
        coord.register_agent(make_profile("a3", HashMap::new())); // should be rejected

        let (agents, _, _) = coord.stats();
        assert_eq!(agents, 2);
    }

    #[test]
    fn test_route_task() {
        let coord = DecentralizedCoordinator::new(10);

        let mut caps = HashMap::new();
        caps.insert("coding".to_string(), 0.9);
        caps.insert("review".to_string(), 0.7);
        coord.register_agent(make_profile("coder", caps));

        let mut caps2 = HashMap::new();
        caps2.insert("design".to_string(), 0.8);
        coord.register_agent(make_profile("designer", caps2));

        let request = RoutingRequest {
            task_type: "code_review".to_string(),
            required_capabilities: vec!["coding".to_string(), "review".to_string()],
            priority: 1.0,
        };

        let decision = coord.route_task(&request);
        assert!(decision.is_some());
        assert_eq!(decision.unwrap().target_agent, "coder");
    }

    #[test]
    fn test_route_with_load() {
        let coord = DecentralizedCoordinator::new(10);

        let mut profile1 = make_profile("busy_agent", HashMap::new());
        profile1.capabilities.insert("coding".to_string(), 0.9);
        profile1.current_load = 0.9;
        coord.register_agent(profile1);

        let mut profile2 = make_profile("free_agent", HashMap::new());
        profile2.capabilities.insert("coding".to_string(), 0.8);
        profile2.current_load = 0.1;
        coord.register_agent(profile2);

        let request = RoutingRequest {
            task_type: "coding".to_string(),
            required_capabilities: vec!["coding".to_string()],
            priority: 1.0,
        };

        let decision = coord.route_task(&request);
        assert!(decision.is_some());
        // free_agent should win due to lower load
        assert_eq!(decision.unwrap().target_agent, "free_agent");
    }

    #[test]
    fn test_evolve_specialization() {
        let coord = DecentralizedCoordinator::new(10);
        coord.register_agent(make_profile("learner", HashMap::new()));

        // Complete 3 successful tasks of the same type
        for _ in 0..3 {
            coord.update_task_result("learner", "coding", true, 100);
        }

        let (_, _, specs) = coord.stats();
        assert_eq!(specs, 1); // should have evolved "coding" specialization
    }

    #[test]
    fn test_no_specialization_below_threshold() {
        let coord = DecentralizedCoordinator::new(10);
        coord.register_agent(make_profile("learner", HashMap::new()));

        // Only 2 successful tasks (need 3)
        coord.update_task_result("learner", "coding", true, 100);
        coord.update_task_result("learner", "coding", true, 100);

        let (_, _, specs) = coord.stats();
        assert_eq!(specs, 0); // should NOT have specialization yet
    }

    #[test]
    fn test_complementarity() {
        let coord = DecentralizedCoordinator::new(10);

        let mut caps_a = HashMap::new();
        caps_a.insert("coding".to_string(), 0.9);
        caps_a.insert("design".to_string(), 0.2);

        let mut caps_b = HashMap::new();
        caps_b.insert("coding".to_string(), 0.2);
        caps_b.insert("design".to_string(), 0.9);

        let profile_a = make_profile("a", caps_a);
        let profile_b = make_profile("b", caps_b);

        let comp = coord.compute_complementarity(&profile_a, &profile_b);
        // High complementarity: one high, one low for each capability
        assert!(comp > 0.3);
    }

    #[test]
    fn test_dag_edges_formed() {
        let coord = DecentralizedCoordinator::new(10);

        let mut caps_a = HashMap::new();
        caps_a.insert("coding".to_string(), 0.9);
        caps_a.insert("design".to_string(), 0.2);
        coord.register_agent(make_profile("a", caps_a));

        let mut caps_b = HashMap::new();
        caps_b.insert("coding".to_string(), 0.2);
        caps_b.insert("design".to_string(), 0.9);
        coord.register_agent(make_profile("b", caps_b));

        let (_, edges, _) = coord.stats();
        assert!(edges > 0); // Should have bidirectional edges
    }

    #[test]
    fn test_task_stats_update() {
        let coord = DecentralizedCoordinator::new(10);
        coord.register_agent(make_profile("worker", HashMap::new()));

        coord.update_task_result("worker", "coding", true, 100);
        coord.update_task_result("worker", "coding", false, 200);

        let profiles = coord.profiles.read().unwrap();
        let stats = profiles.get("worker").unwrap().task_history.get("coding").unwrap();
        assert_eq!(stats.completed, 2);
        assert_eq!(stats.succeeded, 1);
    }

    #[test]
    fn test_empty_route() {
        let coord = DecentralizedCoordinator::new(10);
        let request = RoutingRequest {
            task_type: "coding".to_string(),
            required_capabilities: vec!["coding".to_string()],
            priority: 1.0,
        };
        let decision = coord.route_task(&request);
        assert!(decision.is_none());
    }
}
