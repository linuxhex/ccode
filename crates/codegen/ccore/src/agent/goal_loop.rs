//! Goal Loop — 目标驱动循环（借鉴 Claude Code /goal 设计）
//!
//! 用户指定一个高层目标（如"实现登录模块"），GoalLoop 自动：
//! 1. 将目标拆解为子任务列表
//! 2. 逐个执行子任务（复用 Turn 循环）
//! 3. 每个子任务完成后验证结果
//! 4. 全部完成或无法继续时退出
//!
//! 与 Claude Code 的 /goal 对比：
//! - Claude Code: SKILL.md 内嵌验收清单 + 评估模型判断完成
//! - ccode: GoalSpec 显式定义退出条件 + 验证器评估

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 目标状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalState {
    /// 规划中（LLM 正在拆解子任务）
    Planning,
    /// 执行子任务中
    ExecutingSubtask,
    /// 验证子任务结果
    VerifyingSubtask,
    /// 目标完成
    Completed,
    /// 目标失败
    Failed,
}

/// 子任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTask {
    /// 子任务描述
    pub description: String,
    /// 子任务状态
    pub state: SubTaskState,
    /// 验证条件（自然语言描述，供 LLM 判断）
    pub verification: String,
    /// 尝试次数
    pub attempts: u32,
    /// 最大重试次数
    pub max_retries: u32,
}

/// 子任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubTaskState {
    /// 待执行
    Pending,
    /// 执行中
    InProgress,
    /// 已完成
    Done,
    /// 失败
    Failed,
}

/// 目标完成条件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExitCondition {
    /// 所有子任务完成
    AllSubTasksComplete,
    /// 指定测试通过
    TestsPass {
        /// 测试命令
        command: String,
        /// 期望输出包含的文本
        expected_output: Option<String>,
    },
    /// LLM 评估判断（类似 Claude Code 的评估模型）
    LlmEval {
        /// 评估提示词
        prompt: String,
    },
    /// 混合条件（所有条件都满足）
    All(Vec<ExitCondition>),
}

/// 目标规格
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSpec {
    /// 目标描述
    pub description: String,
    /// 退出条件列表
    pub exit_conditions: Vec<ExitCondition>,
    /// 子任务列表（由 LLM 动态生成或用户预定义）
    pub subtasks: Vec<SubTask>,
    /// 最大总轮次
    pub max_total_turns: u32,
    /// 最大单子任务轮次
    pub max_subtask_turns: u32,
}

/// Goal Loop 状态机
pub struct GoalLoop {
    /// 目标规格
    spec: GoalSpec,
    /// 当前状态
    state: GoalState,
    /// 当前子任务索引
    current_subtask_idx: usize,
    /// 累计轮次
    total_turns: u32,
    /// 当前子任务轮次
    subtask_turns: u32,
    /// 创建时间
    created_at: Instant,
    /// 完成原因
    done_reason: Option<GoalDoneReason>,
}

/// GoalLoop 快照（可序列化，用于持久化恢复）
///
/// 不含 `created_at: Instant`（不可序列化），恢复时重置为当前时间。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalLoopSnapshot {
    /// 当前目标状态
    pub state: GoalState,
    /// 当前子任务索引
    pub current_subtask_idx: usize,
    /// 累计轮次
    pub total_turns: u32,
    /// 当前子任务轮次
    pub subtask_turns: u32,
    /// 目标规格（含子任务列表及其执行状态）
    pub spec: GoalSpec,
    /// 完成原因
    pub done_reason: Option<GoalDoneReason>,
}

/// 目标完成原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GoalDoneReason {
    /// 所有子任务完成
    AllComplete,
    /// 退出条件满足
    ExitConditionMet(String),
    /// 总轮次耗尽
    TotalTurnsExhausted,
    /// 子任务轮次耗尽
    SubTaskTurnsExhausted { subtask: String },
    /// 关键子任务失败
    CriticalSubTaskFailed { subtask: String },
    /// 用户取消
    UserCancelled,
}

/// Goal Loop 输出动作——告诉主循环下一步做什么
#[derive(Debug, Clone)]
pub enum GoalAction {
    /// 执行当前子任务（驱动 Turn 循环）
    ExecuteSubTask {
        /// 子任务描述（注入 WorkingMemory 作为 user 消息）
        description: String,
    },
    /// 验证当前子任务（请求 LLM 判断是否满足验证条件）
    VerifySubTask {
        /// 验证条件
        verification: String,
    },
    /// 目标完成
    GoalComplete {
        reason: GoalDoneReason,
    },
    /// 子任务失败，尝试下一个或重试
    SubTaskFailed {
        subtask_idx: usize,
        will_retry: bool,
    },
}

impl GoalLoop {
    /// 创建新的 GoalLoop
    pub fn new(spec: GoalSpec) -> Self {
        Self {
            state: GoalState::Planning,
            current_subtask_idx: 0,
            total_turns: 0,
            subtask_turns: 0,
            created_at: Instant::now(),
            done_reason: None,
            spec,
        }
    }

    /// 从用户输入创建（自动生成 GoalSpec）
    pub fn from_description(description: String) -> Self {
        Self::new(GoalSpec {
            description,
            exit_conditions: vec![ExitCondition::AllSubTasksComplete],
            subtasks: Vec::new(), // 由 LLM 动态规划
            max_total_turns: 100,
            max_subtask_turns: 20,
        })
    }

    /// 获取当前状态
    pub fn state(&self) -> GoalState {
        self.state
    }

    /// 获取目标描述
    pub fn description(&self) -> &str {
        &self.spec.description
    }

    /// 获取当前子任务索引
    pub fn current_subtask_idx(&self) -> usize {
        self.current_subtask_idx
    }

    /// 获取当前子任务
    pub fn current_subtask(&self) -> Option<&SubTask> {
        self.spec.subtasks.get(self.current_subtask_idx)
    }

    /// 获取总轮次
    pub fn total_turns(&self) -> u32 {
        self.total_turns
    }

    /// 获取完成原因
    pub fn done_reason(&self) -> Option<&GoalDoneReason> {
        self.done_reason.as_ref()
    }

    /// 子任务规划完成（LLM 返回子任务列表后调用）
    pub fn on_subtasks_planned(&mut self, subtasks: Vec<SubTask>) -> GoalAction {
        if subtasks.is_empty() {
            self.state = GoalState::Failed;
            self.done_reason = Some(GoalDoneReason::CriticalSubTaskFailed {
                subtask: "规划失败：无子任务".to_string(),
            });
            return GoalAction::GoalComplete {
                reason: self.done_reason.clone().expect("done_reason must be set before GoalComplete"),
            };
        }
        self.spec.subtasks = subtasks;
        self.state = GoalState::ExecutingSubtask;
        self.current_subtask_idx = 0;
        self.subtask_turns = 0;

        if let Some(subtask) = self.current_subtask() {
            tracing::info!(
                target: "ccore::goal",
                idx = 0,
                total = self.spec.subtasks.len(),
                description = %subtask.description,
                "开始执行子任务"
            );
            GoalAction::ExecuteSubTask {
                description: subtask.description.clone(),
            }
        } else {
            GoalAction::GoalComplete {
                reason: GoalDoneReason::AllComplete,
            }
        }
    }

    /// Turn 循环结束回调（每个子任务的 Turn 结束后调用）
    pub fn on_turn_complete(&mut self, success: bool) -> GoalAction {
        self.total_turns += 1;
        self.subtask_turns += 1;

        // 检查总轮次上限
        if self.total_turns >= self.spec.max_total_turns {
            self.state = GoalState::Failed;
            self.done_reason = Some(GoalDoneReason::TotalTurnsExhausted);
            return GoalAction::GoalComplete {
                reason: self.done_reason.clone().expect("done_reason must be set before GoalComplete"),
            };
        }

        // 检查子任务轮次上限
        if self.subtask_turns >= self.spec.max_subtask_turns {
            return self.handle_subtask_failure("子任务轮次耗尽".to_string());
        }

        if success {
            // 进入验证阶段
            self.state = GoalState::VerifyingSubtask;
            if let Some(subtask) = self.current_subtask() {
                tracing::info!(
                    target: "ccore::goal",
                    idx = self.current_subtask_idx,
                    description = %subtask.description,
                    "子任务执行完成，开始验证"
                );
                GoalAction::VerifySubTask {
                    verification: subtask.verification.clone(),
                }
            } else {
                self.advance_to_next_subtask()
            }
        } else {
            // Turn 失败，继续尝试当前子任务
            self.state = GoalState::ExecutingSubtask;
            if let Some(subtask) = self.current_subtask() {
                GoalAction::ExecuteSubTask {
                    description: subtask.description.clone(),
                }
            } else {
                self.advance_to_next_subtask()
            }
        }
    }

    /// 子任务验证完成回调
    pub fn on_verification_result(&mut self, passed: bool) -> GoalAction {
        if passed {
            // 标记子任务完成
            if let Some(subtask) = self.spec.subtasks.get_mut(self.current_subtask_idx) {
                subtask.state = SubTaskState::Done;
                tracing::info!(
                    target: "ccore::goal",
                    idx = self.current_subtask_idx,
                    description = %subtask.description,
                    "子任务验证通过"
                );
            }
            self.advance_to_next_subtask()
        } else {
            // 验证失败
            if let Some(subtask) = self.spec.subtasks.get_mut(self.current_subtask_idx) {
                subtask.attempts += 1;
                if subtask.attempts >= subtask.max_retries {
                    subtask.state = SubTaskState::Failed;
                }
            }
            self.handle_subtask_failure("验证未通过".to_string())
        }
    }

    /// 生成可序列化快照（用于持久化）
    ///
    /// 不含 `created_at: Instant`（不可序列化），恢复时重置为当前时间。
    pub fn to_snapshot(&self) -> GoalLoopSnapshot {
        GoalLoopSnapshot {
            state: self.state,
            current_subtask_idx: self.current_subtask_idx,
            total_turns: self.total_turns,
            subtask_turns: self.subtask_turns,
            spec: self.spec.clone(),
            done_reason: self.done_reason.clone(),
        }
    }

    /// 从快照恢复状态（created_at 重置为当前时间，开始新的运行周期）
    pub fn restore_from_snapshot(&mut self, snapshot: &GoalLoopSnapshot) {
        self.state = snapshot.state;
        self.current_subtask_idx = snapshot.current_subtask_idx;
        self.total_turns = snapshot.total_turns;
        self.subtask_turns = snapshot.subtask_turns;
        self.spec = snapshot.spec.clone();
        self.done_reason = snapshot.done_reason.clone();
        // created_at 不可序列化，恢复时重置为当前时间
        self.created_at = Instant::now();
    }

    /// 检查退出条件是否满足（由外部调用者提供验证结果）
    pub fn check_exit_conditions(&self, check_results: &[bool]) -> bool {
        if check_results.len() != self.spec.exit_conditions.len() {
            return false;
        }
        check_results.iter().all(|&r| r)
    }

    /// 获取进度信息
    pub fn progress(&self) -> (usize, usize) {
        let completed = self
            .spec
            .subtasks
            .iter()
            .filter(|s| s.state == SubTaskState::Done)
            .count();
        (completed, self.spec.subtasks.len())
    }

    /// 用户取消
    pub fn cancel(&mut self) -> GoalAction {
        self.state = GoalState::Failed;
        self.done_reason = Some(GoalDoneReason::UserCancelled);
        GoalAction::GoalComplete {
            reason: GoalDoneReason::UserCancelled,
        }
    }

    // ---- 内部方法 ----

    /// 前进到下一个子任务
    fn advance_to_next_subtask(&mut self) -> GoalAction {
        // 检查是否所有子任务完成
        let all_done = self.spec.subtasks.iter().all(|s| s.state == SubTaskState::Done);
        if all_done {
            self.state = GoalState::Completed;
            self.done_reason = Some(GoalDoneReason::AllComplete);
            tracing::info!(
                target: "ccore::goal",
                total_turns = self.total_turns,
                elapsed_secs = self.created_at.elapsed().as_secs(),
                "目标完成！"
            );
            return GoalAction::GoalComplete {
                reason: self.done_reason.clone().expect("done_reason must be set before GoalComplete"),
            };
        }

        // 找下一个未完成的子任务
        let next_idx = self.spec.subtasks.iter().position(|s| {
            s.state == SubTaskState::Pending || s.state == SubTaskState::InProgress
        });

        match next_idx {
            Some(idx) => {
                self.current_subtask_idx = idx;
                self.subtask_turns = 0;
                self.state = GoalState::ExecutingSubtask;
                if let Some(subtask) = self.spec.subtasks.get_mut(idx) {
                    subtask.state = SubTaskState::InProgress;
                }
                let description = self.spec.subtasks[idx].description.clone();
                tracing::info!(
                    target: "ccore::goal",
                    idx,
                    total = self.spec.subtasks.len(),
                    description = %description,
                    "开始执行下一个子任务"
                );
                GoalAction::ExecuteSubTask { description }
            }
            None => {
                // 没有更多可执行的子任务（可能有些失败了）
                self.state = GoalState::Completed;
                self.done_reason = Some(GoalDoneReason::AllComplete);
                GoalAction::GoalComplete {
                    reason: self.done_reason.clone().expect("done_reason must be set before GoalComplete"),
                }
            }
        }
    }

    /// 处理子任务失败
    fn handle_subtask_failure(&mut self, reason: String) -> GoalAction {
        let subtask = self.spec.subtasks.get_mut(self.current_subtask_idx);
        let will_retry = subtask.map_or(false, |s| {
            s.attempts < s.max_retries
        });

        if will_retry {
            if let Some(s) = self.spec.subtasks.get_mut(self.current_subtask_idx) {
                s.attempts += 1;
                self.subtask_turns = 0;
                self.state = GoalState::ExecutingSubtask;
            }
            let idx = self.current_subtask_idx;
            tracing::warn!(
                target: "ccore::goal",
                idx,
                reason = %reason,
                "子任务失败，将重试"
            );
            GoalAction::SubTaskFailed {
                subtask_idx: idx,
                will_retry: true,
            }
        } else {
            // 标记失败，前进到下一个
            if let Some(s) = self.spec.subtasks.get_mut(self.current_subtask_idx) {
                s.state = SubTaskState::Failed;
            }
            tracing::warn!(
                target: "ccore::goal",
                idx = self.current_subtask_idx,
                reason = %reason,
                "子任务最终失败"
            );
            self.advance_to_next_subtask()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_subtask(desc: &str, verification: &str) -> SubTask {
        SubTask {
            description: desc.to_string(),
            state: SubTaskState::Pending,
            verification: verification.to_string(),
            attempts: 0,
            max_retries: 2,
        }
    }

    #[test]
    fn test_goal_happy_path() {
        let mut goal = GoalLoop::new(GoalSpec {
            description: "实现登录模块".to_string(),
            exit_conditions: vec![ExitCondition::AllSubTasksComplete],
            subtasks: vec![
                make_subtask("创建 User 模型", "User 结构体存在"),
                make_subtask("实现登录 API", "POST /login 返回 200"),
                make_subtask("添加测试", "测试通过"),
            ],
            max_total_turns: 100,
            max_subtask_turns: 20,
        });

        // 规划完成
        let action = goal.on_subtasks_planned(vec![
            make_subtask("创建 User 模型", "User 结构体存在"),
            make_subtask("实现登录 API", "POST /login 返回 200"),
            make_subtask("添加测试", "测试通过"),
        ]);
        assert!(matches!(action, GoalAction::ExecuteSubTask { .. }));

        // 子任务 0 成功
        let action = goal.on_turn_complete(true);
        assert!(matches!(action, GoalAction::VerifySubTask { .. }));

        // 验证通过
        let action = goal.on_verification_result(true);
        assert!(matches!(action, GoalAction::ExecuteSubTask { .. }));

        // 子任务 1 成功
        goal.on_turn_complete(true);
        goal.on_verification_result(true);

        // 子任务 2 成功
        goal.on_turn_complete(true);
        let action = goal.on_verification_result(true);
        assert!(matches!(action, GoalAction::GoalComplete { .. }));
        assert_eq!(goal.state(), GoalState::Completed);
    }

    #[test]
    fn test_goal_subtask_retry() {
        let mut goal = GoalLoop::from_description("测试重试".to_string());
        goal.spec.subtasks = vec![make_subtask("任务A", "验证A")];
        goal.state = GoalState::ExecutingSubtask;

        // 第一次验证失败
        goal.on_turn_complete(true);
        let action = goal.on_verification_result(false);
        assert!(matches!(action, GoalAction::SubTaskFailed { will_retry: true, .. }));

        // 重试成功
        let action = goal.on_turn_complete(true);
        assert!(matches!(action, GoalAction::VerifySubTask { .. }));
        let action = goal.on_verification_result(true);
        assert!(matches!(action, GoalAction::GoalComplete { .. }));
    }

    #[test]
    fn test_goal_max_turns() {
        let mut goal = GoalLoop::new(GoalSpec {
            description: "测试轮次限制".to_string(),
            exit_conditions: vec![],
            subtasks: vec![make_subtask("无限任务", "永不满足")],
            max_total_turns: 3,
            max_subtask_turns: 20,
        });
        goal.state = GoalState::ExecutingSubtask;

        // 3 轮后耗尽
        goal.on_turn_complete(true);
        goal.on_turn_complete(true);
        let action = goal.on_turn_complete(true);
        assert!(matches!(action, GoalAction::GoalComplete {
            reason: GoalDoneReason::TotalTurnsExhausted
        }));
    }
}
