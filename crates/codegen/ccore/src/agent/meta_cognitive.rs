//! 元认知控制器（借鉴 MAP/LAF 2025 论文）
//!
//! 核心创新：
//! 1. 任务难度自感知：评估任务对当前agent的难度
//! 2. 策略自适应：根据难度选择不同执行策略
//! 3. 冲突监控（MAP）：检测计划中的矛盾和不一致
//! 4. 状态预测（MAP）：预测候选行动的后果
//! 5. 状态评估（MAP）：评分预测状态与目标的匹配度
//!
//! 参考：
//! - MAP: Modular Agentic Planner (Nature Communications 2025)
//! - LAF: LLM-Agent Framework (2025)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

/// 任务难度等级
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd)]
pub enum DifficultyLevel {
    /// 简单：单步工具调用即可完成
    Trivial,
    /// 中等：需要2-5步推理
    Moderate,
    /// 复杂：需要5步以上推理或多文件修改
    Complex,
    /// 极难：需要跨领域知识或多agent协作
    Extreme,
}

/// 执行策略
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ExecutionStrategy {
    /// 直接执行：简单任务直接调用工具
    Direct,
    /// 计划后执行：中等任务先制定计划
    PlanThenExecute,
    /// 反思式执行：复杂任务用ReAct+反思循环
    ReflectiveExecution,
    /// 多agent协作：极难任务委派给子agent
    MultiAgent,
}

/// 冲突类型（MAP: Conflict Monitoring）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConflictType {
    /// 目标冲突：两个子目标相互矛盾
    GoalConflict,
    /// 资源冲突：两个操作竞争同一资源
    ResourceConflict,
    /// 时序冲突：操作顺序依赖被违反
    TemporalConflict,
    /// 逻辑冲突：推理链自相矛盾
    LogicalConflict,
}

/// 冲突检测结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConflictDetection {
    pub conflict_type: ConflictType,
    pub description: String,
    pub severity: f64, // 0.0-1.0
    pub suggestion: String,
}

/// 状态预测结果（MAP: State Prediction）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatePrediction {
    /// 预测的行动
    pub action: String,
    /// 预测的后果
    pub predicted_outcome: String,
    /// 置信度
    pub confidence: f64,
    /// 风险评估
    pub risk_level: f64,
}

/// 元认知控制器
pub struct MetaCognitiveController {
    /// 历史难度评估
    difficulty_history: RwLock<Vec<(String, DifficultyLevel)>>,
    /// 策略效果追踪
    strategy_effectiveness: RwLock<HashMap<String, f64>>,
}

impl MetaCognitiveController {
    pub fn new() -> Self {
        Self {
            difficulty_history: RwLock::new(Vec::new()),
            strategy_effectiveness: RwLock::new(HashMap::new()),
        }
    }

    /// 任务难度自感知（LAF: Meta-Controller核心）
    pub fn assess_difficulty(
        &self,
        task: &str,
        context: &HashMap<String, String>,
    ) -> DifficultyLevel {
        let mut score = 0.0;

        // 信号1：步骤数量估计
        let step_signals = ["然后", "接着", "之后", "同时", "并且", "另外"];
        let step_count = step_signals.iter().filter(|s| task.contains(*s)).count();
        score += step_count as f64 * 0.15;

        // 信号2：跨文件信号
        let multi_file_signals = ["多个文件", "所有文件", "整个项目", "跨模块"];
        for signal in multi_file_signals {
            if task.contains(signal) {
                score += 0.2;
            }
        }

        // 信号3：上下文复杂度
        if let Some(file_count) = context.get("relevant_files") {
            if let Ok(count) = file_count.parse::<usize>() {
                score += (count as f64 / 5.0).min(0.3);
            }
        }

        // 信号4：历史相似任务的难度
        let history = self.difficulty_history.read().unwrap();
        for (past_task, past_difficulty) in history.iter() {
            let similarity = self.trigram_similarity(task, past_task);
            if similarity > 0.3 {
                let past_score = match past_difficulty {
                    DifficultyLevel::Trivial => 0.0,
                    DifficultyLevel::Moderate => 0.3,
                    DifficultyLevel::Complex => 0.6,
                    DifficultyLevel::Extreme => 0.9,
                };
                score += past_score * similarity * 0.3;
            }
        }

        let level = if score < 0.2 {
            DifficultyLevel::Trivial
        } else if score < 0.4 {
            DifficultyLevel::Moderate
        } else if score < 0.7 {
            DifficultyLevel::Complex
        } else {
            DifficultyLevel::Extreme
        };

        tracing::debug!(
            target: "ccore::meta",
            score = score,
            level = ?level,
            "difficulty assessed"
        );

        // 记录历史
        drop(history);
        self.difficulty_history
            .write()
            .unwrap()
            .push((task.to_string(), level.clone()));

        level
    }

    /// 策略选择（LAF: 根据难度选择执行策略）
    pub fn select_strategy(&self, difficulty: &DifficultyLevel) -> ExecutionStrategy {
        match difficulty {
            DifficultyLevel::Trivial => ExecutionStrategy::Direct,
            DifficultyLevel::Moderate => ExecutionStrategy::PlanThenExecute,
            DifficultyLevel::Complex => ExecutionStrategy::ReflectiveExecution,
            DifficultyLevel::Extreme => ExecutionStrategy::MultiAgent,
        }
    }

    /// 冲突监控（MAP: Conflict Monitoring）
    pub fn detect_conflicts(&self, plan: &[String]) -> Vec<ConflictDetection> {
        let mut conflicts = Vec::new();

        // 检测目标冲突
        let conflicting_patterns = [
            ("添加", "删除"),
            ("创建", "移除"),
            ("增加", "减少"),
            ("开启", "关闭"),
        ];

        for (i, step_a) in plan.iter().enumerate() {
            for (j, step_b) in plan.iter().enumerate() {
                if i >= j {
                    continue;
                }
                for (pattern_a, pattern_b) in &conflicting_patterns {
                    if step_a.contains(pattern_a) && step_b.contains(pattern_b) {
                        conflicts.push(ConflictDetection {
                            conflict_type: ConflictType::GoalConflict,
                            description: format!(
                                "步骤{}和{}可能冲突：'{}' vs '{}'",
                                i + 1,
                                j + 1,
                                step_a,
                                step_b
                            ),
                            severity: 0.7,
                            suggestion: "请确认这两个操作是否作用于不同目标".to_string(),
                        });
                    }
                }
            }
        }

        // 检测时序冲突
        let dependency_patterns = [("先", "后"), ("之前", "之后"), ("前提", "结果")];
        for (i, step) in plan.iter().enumerate() {
            for (dep_a, _dep_b) in &dependency_patterns {
                if step.contains(dep_a) && i > 0 {
                    conflicts.push(ConflictDetection {
                        conflict_type: ConflictType::TemporalConflict,
                        description: format!("步骤{}可能有时序依赖问题", i + 1),
                        severity: 0.5,
                        suggestion: "请确认执行顺序".to_string(),
                    });
                }
            }
        }

        if !conflicts.is_empty() {
            tracing::warn!(
                target: "ccore::meta",
                conflicts = conflicts.len(),
                "conflicts detected in plan"
            );
        }

        conflicts
    }

    /// 状态预测（MAP: State Prediction）
    pub fn predict_outcome(&self, action: &str, _current_state: &str) -> StatePrediction {
        // 简化版：基于关键词的启发式预测
        let (predicted_outcome, confidence, risk) =
            if action.contains("删除") || action.contains("rm") {
                ("文件将被永久删除，不可恢复".to_string(), 0.9, 0.8)
            } else if action.contains("修改") || action.contains("edit") {
                ("文件内容将被修改".to_string(), 0.8, 0.4)
            } else if action.contains("创建") || action.contains("write") {
                ("新文件将被创建".to_string(), 0.9, 0.2)
            } else if action.contains("运行") || action.contains("exec") {
                ("命令将被执行，可能有副作用".to_string(), 0.7, 0.6)
            } else {
                ("操作将执行".to_string(), 0.5, 0.3)
            };

        StatePrediction {
            action: action.to_string(),
            predicted_outcome,
            confidence,
            risk_level: risk,
        }
    }

    /// 状态评估（MAP: State Evaluation）
    ///
    /// 评分预测状态与目标匹配度
    pub fn evaluate_state(&self, predicted_state: &str, goal: &str) -> f64 {
        self.trigram_similarity(predicted_state, goal)
    }

    /// 更新策略效果（闭环学习）
    pub fn update_strategy_effectiveness(
        &self,
        strategy: &ExecutionStrategy,
        task_success: bool,
    ) {
        let mut effects = self.strategy_effectiveness.write().unwrap();
        let key = format!("{:?}", strategy);
        let delta = if task_success { 0.1 } else { -0.15 };
        let current = effects.entry(key).or_insert(0.5);
        *current = (*current + delta).clamp(0.0, 1.0);
    }

    fn trigram_similarity(&self, a: &str, b: &str) -> f64 {
        crate::utils::trigram_similarity(a, b)
    }

    /// 获取统计信息
    pub fn stats(&self) -> (usize, HashMap<String, f64>) {
        let history = self.difficulty_history.read().unwrap();
        let effects = self.strategy_effectiveness.read().unwrap();
        (history.len(), effects.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assess_trivial_difficulty() {
        let ctrl = MetaCognitiveController::new();
        let ctx = HashMap::new();
        let level = ctrl.assess_difficulty("读取文件内容", &ctx);
        assert_eq!(level, DifficultyLevel::Trivial);
    }

    #[test]
    fn test_assess_moderate_difficulty() {
        let ctrl = MetaCognitiveController::new();
        let ctx = HashMap::new();
        // "然后" + "接着" = 2 steps * 0.15 = 0.3 >= 0.2 (Moderate threshold)
        let level = ctrl.assess_difficulty("读取文件然后修改内容接着运行测试", &ctx);
        assert!(level >= DifficultyLevel::Moderate);
    }

    #[test]
    fn test_assess_complex_difficulty() {
        let ctrl = MetaCognitiveController::new();
        let mut ctx = HashMap::new();
        ctx.insert("relevant_files".to_string(), "8".to_string());
        let level = ctrl.assess_difficulty(
            "重构整个项目然后添加测试接着修改配置之后更新文档同时处理依赖",
            &ctx,
        );
        assert!(level >= DifficultyLevel::Complex);
    }

    #[test]
    fn test_difficulty_with_history() {
        let ctrl = MetaCognitiveController::new();
        let ctx = HashMap::new();

        // First assessment
        ctrl.assess_difficulty("重构整个项目然后添加测试接着修改配置", &ctx);

        // Similar task should inherit difficulty
        let level = ctrl.assess_difficulty("重构整个项目然后添加测试接着修改配置", &ctx);
        assert!(level >= DifficultyLevel::Complex);
    }

    #[test]
    fn test_strategy_selection() {
        let ctrl = MetaCognitiveController::new();

        assert_eq!(
            ctrl.select_strategy(&DifficultyLevel::Trivial),
            ExecutionStrategy::Direct
        );
        assert_eq!(
            ctrl.select_strategy(&DifficultyLevel::Moderate),
            ExecutionStrategy::PlanThenExecute
        );
        assert_eq!(
            ctrl.select_strategy(&DifficultyLevel::Complex),
            ExecutionStrategy::ReflectiveExecution
        );
        assert_eq!(
            ctrl.select_strategy(&DifficultyLevel::Extreme),
            ExecutionStrategy::MultiAgent
        );
    }

    #[test]
    fn test_detect_goal_conflicts() {
        let ctrl = MetaCognitiveController::new();
        let plan = vec![
            "添加新功能".to_string(),
            "删除旧功能".to_string(),
        ];
        let conflicts = ctrl.detect_conflicts(&plan);
        assert!(!conflicts.is_empty());
        assert!(conflicts
            .iter()
            .any(|c| matches!(c.conflict_type, ConflictType::GoalConflict)));
    }

    #[test]
    fn test_no_conflicts() {
        let ctrl = MetaCognitiveController::new();
        let plan = vec![
            "读取文件".to_string(),
            "分析内容".to_string(),
        ];
        let conflicts = ctrl.detect_conflicts(&plan);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_detect_temporal_conflict() {
        let ctrl = MetaCognitiveController::new();
        // "先" is in dependency_patterns as dep_a, so if a step after the first contains "先" it triggers
        let plan = vec![
            "初始化环境".to_string(),
            "先完成测试再提交".to_string(),
        ];
        let conflicts = ctrl.detect_conflicts(&plan);
        assert!(conflicts
            .iter()
            .any(|c| matches!(c.conflict_type, ConflictType::TemporalConflict)));
    }

    #[test]
    fn test_predict_delete_outcome() {
        let ctrl = MetaCognitiveController::new();
        let pred = ctrl.predict_outcome("删除文件", "初始状态");
        assert!(pred.risk_level > 0.5);
        assert!(pred.confidence > 0.5);
    }

    #[test]
    fn test_predict_create_outcome() {
        let ctrl = MetaCognitiveController::new();
        let pred = ctrl.predict_outcome("创建文件", "初始状态");
        assert!(pred.risk_level < 0.5);
    }

    #[test]
    fn test_evaluate_state() {
        let ctrl = MetaCognitiveController::new();
        let score = ctrl.evaluate_state("文件已成功创建", "文件已成功创建");
        assert!(score > 0.5);

        let score2 = ctrl.evaluate_state("完全无关的内容", "另一个不同的目标");
        assert!(score2 < score);
    }

    #[test]
    fn test_update_strategy_effectiveness() {
        let ctrl = MetaCognitiveController::new();

        ctrl.update_strategy_effectiveness(&ExecutionStrategy::Direct, true);
        ctrl.update_strategy_effectiveness(&ExecutionStrategy::Direct, true);

        let (_, effects) = ctrl.stats();
        let direct_eff = effects.get("Direct").unwrap();
        assert!(*direct_eff > 0.5);
    }

    #[test]
    fn test_strategy_effectiveness_clamps() {
        let ctrl = MetaCognitiveController::new();

        // Drive effectiveness down
        for _ in 0..20 {
            ctrl.update_strategy_effectiveness(&ExecutionStrategy::MultiAgent, false);
        }

        let (_, effects) = ctrl.stats();
        let multi_eff = effects.get("MultiAgent").unwrap();
        assert!(*multi_eff >= 0.0);
    }

    #[test]
    fn test_trigram_similarity() {
        let ctrl = MetaCognitiveController::new();
        let sim = ctrl.trigram_similarity("Rust编程语言", "Rust编程语言是安全的");
        assert!(sim > 0.0);

        let sim2 = ctrl.trigram_similarity("完全不同", "毫无关系");
        assert!(sim2 < sim);
    }

    #[test]
    fn test_empty_strings_similarity() {
        let ctrl = MetaCognitiveController::new();
        let sim = ctrl.trigram_similarity("", "有内容");
        assert_eq!(sim, 0.0);
    }
}
