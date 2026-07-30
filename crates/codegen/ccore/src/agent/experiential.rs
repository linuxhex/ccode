//! 经验反思学习模块（借鉴 ERL/MAR 2025-2026 论文）
//!
//! 核心创新：
//! 1. ERL heuristic提取：从任务轨迹提取可复用的经验教训
//! 2. MAR多角色反思：Verifier/Planner/Skeptic/Logician多视角诊断
//! 3. 按任务相关性检索heuristic注入上下文
//!
//! 参考：
//! - ERL: Experiential Reflective Learning for Self-Improving LLM Agents (ICLR 2026)
//! - MAR: Multi-Agent Reflexion Improves Reasoning Abilities in LLMs (2025)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// 经验启发（ERL: heuristic）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heuristic {
    /// 唯一ID
    pub id: String,
    /// 启发内容（可复用的经验教训）
    pub content: String,
    /// 来源任务类型
    pub source_task_type: String,
    /// 来源结果：成功/失败
    pub from_success: bool,
    /// 适用场景描述
    pub applies_to: String,
    /// 相关性评分（ERL: selective retrieval）
    pub relevance_score: f64,
    /// 使用次数
    pub usage_count: u64,
    /// 有效性统计
    pub effectiveness: f64,
}

/// 反思角色（MAR: diverse reasoning personas）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ReflectionPersona {
    /// 验证者：检查事实准确性
    Verifier,
    /// 规划者：评估计划合理性
    Planner,
    /// 怀疑者：质疑假设和逻辑
    Skeptic,
    /// 逻辑者：检查推理链完整性
    Logician,
    /// 元反思者：综合所有视角
    MetaReflector,
}

impl ReflectionPersona {
    /// 获取角色提示模板
    pub fn prompt_template(&self) -> &str {
        match self {
            Self::Verifier => r#"你是一个验证者(Verifier)。你的职责是：
1. 检查每一步的事实准确性
2. 验证工具调用的参数是否正确
3. 确认返回结果是否被正确解读
4. 标记任何未经证实就接受的假设

请审查以下轨迹，指出事实性错误："#,
            Self::Planner => r#"你是一个规划者(Planner)。你的职责是：
1. 评估整体计划的合理性
2. 检查步骤顺序是否最优
3. 识别可以并行的步骤
4. 标记不必要的绕路

请审查以下轨迹，指出规划问题："#,
            Self::Skeptic => r#"你是一个怀疑者(Skeptic)。你的职责是：
1. 质疑每一个隐含假设
2. 寻找被忽略的边界情况
3. 挑战"显而易见"的结论
4. 标记确认偏误的迹象

请审查以下轨迹，指出被忽视的风险："#,
            Self::Logician => r#"你是一个逻辑者(Logician)。你的职责是：
1. 检查推理链的完整性
2. 识别跳跃性推理
3. 标记循环论证
4. 确保因果关系成立

请审查以下轨迹，指出逻辑漏洞："#,
            Self::MetaReflector => r#"你是一个元反思者(MetaReflector)。你的职责是：
1. 综合其他角色的诊断
2. 提炼可复用的经验教训
3. 生成具体的改进行动
4. 评估反思本身的质量

请综合以下诊断，生成经验教训："#,
        }
    }
}

/// 任务轨迹
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTrajectory {
    /// 任务描述
    pub task: String,
    /// 执行步骤
    pub steps: Vec<TrajectoryStep>,
    /// 最终结果：成功/失败
    pub success: bool,
    /// 失败原因（如果失败）
    pub failure_reason: Option<String>,
}

/// 轨迹步骤
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    /// 步骤类型
    pub step_type: String, // "think", "tool_call", "tool_result", "observation"
    /// 内容
    pub content: String,
}

/// 经验反思学习引擎
pub struct ExperientialReflectiveLearner {
    /// heuristic池
    heuristics: RwLock<Vec<Heuristic>>,
    /// 最大heuristic数量
    max_heuristics: usize,
}

impl ExperientialReflectiveLearner {
    pub fn new(max_heuristics: usize) -> Self {
        Self {
            heuristics: RwLock::new(Vec::new()),
            max_heuristics,
        }
    }

    fn generate_heuristic_id() -> String {
        format!(
            "h_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        )
    }

    /// 从任务轨迹提取heuristic（ERL核心算法）
    ///
    /// ERL: 反思任务轨迹和结果，生成可复用的经验教训
    pub fn extract_heuristics(&self, trajectory: &TaskTrajectory) -> Vec<Heuristic> {
        let mut extracted = Vec::new();

        // 成功轨迹：提取有效策略
        if trajectory.success {
            for step in &trajectory.steps {
                if step.step_type == "tool_call" {
                    let heuristic = Heuristic {
                        id: Self::generate_heuristic_id(),
                        content: format!(
                            "在'{}'类任务中，使用'{}'是有效的",
                            trajectory.task, step.content
                        ),
                        source_task_type: trajectory.task.clone(),
                        from_success: true,
                        applies_to: trajectory.task.clone(),
                        relevance_score: 0.5,
                        usage_count: 0,
                        effectiveness: 0.5,
                    };
                    extracted.push(heuristic);
                }
            }
        } else {
            // 失败轨迹：提取失败模式
            if let Some(reason) = &trajectory.failure_reason {
                let heuristic = Heuristic {
                    id: Self::generate_heuristic_id(),
                    content: format!(
                        "在'{}'类任务中，'{}'会导致失败。避免此模式。",
                        trajectory.task, reason
                    ),
                    source_task_type: trajectory.task.clone(),
                    from_success: false,
                    applies_to: trajectory.task.clone(),
                    relevance_score: 0.7, // 失败教训通常更有价值
                    usage_count: 0,
                    effectiveness: 0.5,
                };
                extracted.push(heuristic);
            }
        }

        // 存入heuristic池
        let mut pool = self.heuristics.write().unwrap();
        for h in &extracted {
            if pool.len() < self.max_heuristics {
                pool.push(h.clone());
            } else {
                // 替换最低效的
                if let Some(min_idx) = pool
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        a.effectiveness
                            .partial_cmp(&b.effectiveness)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i)
                {
                    if h.effectiveness > pool[min_idx].effectiveness {
                        pool[min_idx] = h.clone();
                    }
                }
            }
        }

        tracing::info!(
            target: "ccore::erl",
            extracted = extracted.len(),
            pool_size = pool.len(),
            success = trajectory.success,
            "heuristics extracted from trajectory"
        );

        extracted
    }

    /// 检索相关heuristic（ERL: selective retrieval）
    ///
    /// 根据当前任务检索最相关的heuristic注入上下文
    pub fn retrieve_relevant(&self, task: &str, top_k: usize) -> Vec<Heuristic> {
        let pool = self.heuristics.read().unwrap();
        let mut scored: Vec<(usize, f64)> = Vec::new();

        for (i, h) in pool.iter().enumerate() {
            let mut score = 0.0;

            // 任务类型匹配
            if task.contains(&h.source_task_type) || h.source_task_type.contains(task) {
                score += 0.4;
            }

            // 适用场景匹配
            if task.contains(&h.applies_to) {
                score += 0.3;
            }

            // trigram相似度
            let trigrams_task: std::collections::HashSet<_> = task
                .chars()
                .collect::<Vec<_>>()
                .windows(3)
                .map(|w| w.iter().collect::<String>())
                .collect();
            let trigrams_h: std::collections::HashSet<_> = h
                .content
                .chars()
                .collect::<Vec<_>>()
                .windows(3)
                .map(|w| w.iter().collect::<String>())
                .collect();

            if !trigrams_task.is_empty() && !trigrams_h.is_empty() {
                let intersection = trigrams_task.intersection(&trigrams_h).count() as f64;
                let union = trigrams_task.union(&trigrams_h).count() as f64;
                score += (intersection / union) * 0.3;
            }

            // 有效性加权
            score *= h.effectiveness;

            if score > 0.1 {
                scored.push((i, score));
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .take(top_k)
            .filter_map(|(i, score)| {
                let mut h = pool[i].clone();
                h.relevance_score = score;
                Some(h)
            })
            .collect()
    }

    /// 更新heuristic有效性（ERL: 闭环反馈）
    pub fn update_effectiveness(&self, heuristic_id: &str, was_helpful: bool) {
        let mut pool = self.heuristics.write().unwrap();
        if let Some(h) = pool.iter_mut().find(|h| h.id == heuristic_id) {
            h.usage_count += 1;
            let delta = if was_helpful { 0.1 } else { -0.15 };
            h.effectiveness = (h.effectiveness + delta).clamp(0.0, 1.0);
        }
    }

    /// 多角色反思（MAR: Multi-Agent Reflexion）
    ///
    /// 使用多个推理角色诊断失败轨迹
    pub fn multi_persona_reflect(
        &self,
        trajectory: &TaskTrajectory,
    ) -> HashMap<ReflectionPersona, String> {
        let personas = [
            ReflectionPersona::Verifier,
            ReflectionPersona::Planner,
            ReflectionPersona::Skeptic,
            ReflectionPersona::Logician,
        ];

        let mut reflections = HashMap::new();
        let trajectory_summary = self.summarize_trajectory(trajectory);

        for persona in personas {
            // 在实际实现中，这里会调用LLM
            // 目前返回模板化的反思
            let reflection = format!(
                "[{}] 审查任务'{}'的{}轨迹：\n{}\n\n建议：请根据{}视角分析此轨迹",
                match persona {
                    ReflectionPersona::Verifier => "验证者",
                    ReflectionPersona::Planner => "规划者",
                    ReflectionPersona::Skeptic => "怀疑者",
                    ReflectionPersona::Logician => "逻辑者",
                    ReflectionPersona::MetaReflector => "元反思者",
                },
                trajectory.task,
                if trajectory.success { "成功" } else { "失败" },
                trajectory_summary,
                match persona {
                    ReflectionPersona::Verifier => "事实准确性",
                    ReflectionPersona::Planner => "规划合理性",
                    ReflectionPersona::Skeptic => "潜在风险",
                    ReflectionPersona::Logician => "逻辑完整性",
                    ReflectionPersona::MetaReflector => "综合改进",
                },
            );
            reflections.insert(persona, reflection);
        }

        // MetaReflector综合所有视角
        let meta = ReflectionPersona::MetaReflector;
        let all_reflections: Vec<String> = reflections.values().cloned().collect();
        reflections.insert(
            meta,
            format!(
                "[元反思者] 综合{}个视角的诊断：\n{}\n\n核心教训：需要进一步LLM调用生成具体改进建议",
                reflections.len(),
                all_reflections.join("\n---\n")
            ),
        );

        reflections
    }

    /// 生成heuristic注入文本（ERL: inject into context）
    pub fn format_for_injection(&self, heuristics: &[Heuristic]) -> String {
        if heuristics.is_empty() {
            return String::new();
        }

        let mut parts = vec!["[相关经验教训]".to_string()];
        for h in heuristics {
            let marker = if h.from_success {
                "✓ 有效策略"
            } else {
                "✗ 失败教训"
            };
            parts.push(format!("{} {}: {}", marker, h.applies_to, h.content));
        }
        parts.join("\n")
    }

    fn summarize_trajectory(&self, trajectory: &TaskTrajectory) -> String {
        let mut summary = Vec::new();
        for (i, step) in trajectory.steps.iter().enumerate() {
            summary.push(format!("步骤{} [{}]: {}", i + 1, step.step_type, step.content));
        }
        if let Some(reason) = &trajectory.failure_reason {
            summary.push(format!("失败原因: {}", reason));
        }
        summary.join("\n")
    }

    /// 获取统计信息
    pub fn stats(&self) -> (usize, usize, f64) {
        let pool = self.heuristics.read().unwrap();
        let from_success = pool.iter().filter(|h| h.from_success).count();
        let avg_effectiveness = if pool.is_empty() {
            0.0
        } else {
            pool.iter().map(|h| h.effectiveness).sum::<f64>() / pool.len() as f64
        };
        (pool.len(), from_success, avg_effectiveness)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_success_trajectory() -> TaskTrajectory {
        TaskTrajectory {
            task: "重构代码".to_string(),
            steps: vec![
                TrajectoryStep {
                    step_type: "think".to_string(),
                    content: "需要先分析依赖关系".to_string(),
                },
                TrajectoryStep {
                    step_type: "tool_call".to_string(),
                    content: "grep查找所有引用".to_string(),
                },
                TrajectoryStep {
                    step_type: "tool_result".to_string(),
                    content: "找到5处引用".to_string(),
                },
            ],
            success: true,
            failure_reason: None,
        }
    }

    fn make_failure_trajectory() -> TaskTrajectory {
        TaskTrajectory {
            task: "修复bug".to_string(),
            steps: vec![
                TrajectoryStep {
                    step_type: "think".to_string(),
                    content: "需要定位bug".to_string(),
                },
                TrajectoryStep {
                    step_type: "tool_call".to_string(),
                    content: "运行测试".to_string(),
                },
            ],
            success: false,
            failure_reason: Some("测试超时".to_string()),
        }
    }

    #[test]
    fn test_extract_from_success() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_success_trajectory();
        let extracted = learner.extract_heuristics(&trajectory);

        assert!(!extracted.is_empty());
        assert!(extracted.iter().all(|h| h.from_success));
    }

    #[test]
    fn test_extract_from_failure() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_failure_trajectory();
        let extracted = learner.extract_heuristics(&trajectory);

        assert!(!extracted.is_empty());
        assert!(extracted.iter().all(|h| !h.from_success));
    }

    #[test]
    fn test_retrieve_relevant() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_success_trajectory();
        learner.extract_heuristics(&trajectory);

        let relevant = learner.retrieve_relevant("重构代码", 5);
        assert!(!relevant.is_empty());
    }

    #[test]
    fn test_update_effectiveness() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_success_trajectory();
        let extracted = learner.extract_heuristics(&trajectory);

        let h_id = &extracted[0].id;
        learner.update_effectiveness(h_id, true);
        learner.update_effectiveness(h_id, true);

        let (_, _, avg) = learner.stats();
        assert!(avg > 0.5);
    }

    #[test]
    fn test_multi_persona_reflect() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_failure_trajectory();
        let reflections = learner.multi_persona_reflect(&trajectory);

        assert_eq!(reflections.len(), 5); // 4 + MetaReflector
        assert!(reflections.contains_key(&ReflectionPersona::Verifier));
        assert!(reflections.contains_key(&ReflectionPersona::MetaReflector));
    }

    #[test]
    fn test_format_for_injection() {
        let learner = ExperientialReflectiveLearner::new(100);
        let trajectory = make_success_trajectory();
        let extracted = learner.extract_heuristics(&trajectory);

        let text = learner.format_for_injection(&extracted);
        assert!(text.contains("相关经验教训"));
        assert!(text.contains("有效策略"));
    }

    #[test]
    fn test_format_empty() {
        let learner = ExperientialReflectiveLearner::new(100);
        let text = learner.format_for_injection(&[]);
        assert!(text.is_empty());
    }

    #[test]
    fn test_max_heuristics_limit() {
        let learner = ExperientialReflectiveLearner::new(2);
        for _ in 0..5 {
            learner.extract_heuristics(&make_success_trajectory());
        }
        let (count, _, _) = learner.stats();
        assert!(count <= 2);
    }

    #[test]
    fn test_persona_prompt_templates() {
        assert!(ReflectionPersona::Verifier.prompt_template().contains("验证者"));
        assert!(ReflectionPersona::Planner.prompt_template().contains("规划者"));
        assert!(ReflectionPersona::Skeptic.prompt_template().contains("怀疑者"));
        assert!(ReflectionPersona::Logician.prompt_template().contains("逻辑者"));
        assert!(ReflectionPersona::MetaReflector.prompt_template().contains("元反思者"));
    }
}
