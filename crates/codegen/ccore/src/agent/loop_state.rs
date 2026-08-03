//! Agent 主循环状态机（借鉴 Claude Code queryLoop 设计，适配 ccore 架构）
//!
//! 将主循环从隐式 `loop {}` 升级为显式状态机驱动，使状态变迁可观测、可中断、可恢复。
//! 使用 ccore 已有的 AgentState 枚举表示当前阶段，并集成 DoomLoopDetector 的检测结果。
//!
//! 与 ExperientialReflectiveLearner 的集成：当循环结束时（成功或失败），
//! 从状态机中提取 TaskTrajectory，供 ERL 提取 heuristics 使用。

use serde::{Deserialize, Serialize};
use std::time::Instant;
use crate::persistence::state::LoopStateSnapshot;

use super::doom_loop::DoomLoopResult;
use super::experiential::{TaskTrajectory, TrajectoryStep};

/// 循环结束原因
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoneReason {
    /// 模型返回 end_turn，正常结束
    ModelEndTurn,
    /// 用户取消
    UserCancelled,
    /// Token 预算耗尽
    BudgetExhausted,
    /// 工具调用预算耗尽
    ToolBudgetExhausted,
    /// 循环检测熔断
    LoopDetected {
        tool_name: String,
        repeat_count: usize,
    },
    /// 连续失败熔断
    ConsecutiveFailures {
        count: usize,
    },
    /// 权限被拒绝且用户不重试
    PermissionDenied {
        tool_name: String,
    },
    /// 模型拒绝
    ModelRefusal,
    /// 最大轮次限制
    MaxTurnsReached,
}

/// 驱动状态机转换的事件
#[derive(Debug, Clone)]
pub enum LoopEvent {
    /// LLM 返回了响应
    LLMResponse {
        /// stop_reason: "end_turn" | "tool_use" | "max_tokens" | ...
        stop_reason: String,
        /// 本轮消耗的 token 数
        token_used: u64,
        /// 工具调用列表（id, name, input）
        tool_calls: Vec<(String, String, serde_json::Value)>,
    },
    /// 工具执行完成
    ToolExecutionCompleted {
        tool_use_id: String,
        tool_name: String,
        result: ToolExecutionOutcome,
    },
    /// 权限决策
    PermissionDecision {
        tool_name: String,
        tool_use_id: String,
        allowed: bool,
    },
    /// 用户取消
    UserCancelled,
    /// 预算耗尽
    BudgetExhausted {
        kind: BudgetKind,
    },
    /// DoomLoop 检测触发（从 DoomLoopResult 转换）
    LoopDetected {
        tool_name: String,
        repeat_count: usize,
    },
    /// 连续失败达到阈值
    ConsecutiveFailures {
        count: usize,
    },
    /// 最大轮次
    MaxTurnsReached,
}

impl LoopEvent {
    /// 从 DoomLoopResult 构造 LoopDetected 事件
    pub fn from_doom_loop(result: &DoomLoopResult) -> Option<Self> {
        if result.detected {
            Some(LoopEvent::LoopDetected {
                tool_name: result.repeated_tool.clone().unwrap_or_default(),
                repeat_count: result.repeat_count,
            })
        } else {
            None
        }
    }
}

/// 预算类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetKind {
    Token,
    ToolCall,
}

/// 工具执行结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolExecutionOutcome {
    Success,
    Failure(String),
}

/// 状态机输出的动作——告诉主循环下一步做什么
#[derive(Debug, Clone)]
pub enum LoopAction {
    /// 调用 LLM（主循环执行下一轮对话）
    CallLLM,
    /// 执行工具（主循环继续处理工具调用）
    ExecuteTool {
        tool_name: String,
        tool_use_id: String,
    },
    /// 等待权限确认
    WaitForPermission {
        tool_name: String,
        tool_use_id: String,
    },
    /// 结束循环
    EndTurn {
        reason: DoneReason,
    },
    /// 继续循环（条件不满足，继续下一轮）
    ContinueLoop,
}

/// 自动压缩阈值常量
const AUTO_COMPACT_THRESHOLD_RATIO: f64 = 0.8;

/// 主循环状态机
///
/// 使用 ccore 已有的 AgentState 表示当前阶段，
/// 追踪 turn_count / tokens_used / consecutive_failures / created_at，
/// 并在状态变迁时输出 LoopAction 指导主循环执行。
///
/// 集成 ExperientialReflectiveLearner：在循环执行过程中收集步骤轨迹，
/// 循环结束时可通过 extract_trajectory() 提取 TaskTrajectory 供 ERL 使用。
pub struct LoopStateMachine {
    /// 当前状态
    state: super::AgentState,
    /// 结束原因（仅在 Done/Error 状态时有值）
    done_reason: Option<DoneReason>,
    /// 当前轮次（从 0 开始，每次 CallLLM 后递增）
    turn_count: u32,
    /// 累计消耗的 token
    tokens_used: u64,
    /// 连续工具失败次数
    consecutive_failures: usize,
    /// 创建时间
    created_at: Instant,
    /// 收集的步骤轨迹（供 ERL 提取 heuristics）
    collected_steps: Vec<TrajectoryStep>,
    /// 任务描述（用于 ERL trajectory）
    task_description: Option<String>,
}

impl LoopStateMachine {
    /// 创建新的状态机，初始状态为 Idle
    pub fn new() -> Self {
        Self {
            state: super::AgentState::Idle,
            done_reason: None,
            turn_count: 0,
            tokens_used: 0,
            consecutive_failures: 0,
            created_at: Instant::now(),
            collected_steps: Vec::new(),
            task_description: None,
        }
    }

    /// 生成可序列化快照（不含 GoalLoop，由调用方填充）
    ///
    /// GoalLoop 状态由 ThinkerNode 单独序列化后填充到 snapshot.goal 字段。
    pub fn to_snapshot(&self) -> LoopStateSnapshot {
        LoopStateSnapshot {
            state: self.state,
            turn_count: self.turn_count,
            tokens_used: self.tokens_used,
            consecutive_failures: self.consecutive_failures as u32,
            elapsed_secs: self.created_at.elapsed().as_secs(),
            done_reason: self.done_reason.clone(),
            goal: None, // 由 ThinkerNode 填充
        }
    }

    /// 从快照恢复 Turn 级状态
    ///
    /// created_at 重置为当前时间（Instant 不可序列化）。
    /// collected_steps / task_description 不恢复（运行时收集的数据，新周期重新积累）。
    pub fn restore_from_snapshot(&mut self, snapshot: &LoopStateSnapshot) {
        self.state = snapshot.state;
        self.turn_count = snapshot.turn_count;
        self.tokens_used = snapshot.tokens_used;
        self.consecutive_failures = snapshot.consecutive_failures as usize;
        self.done_reason = snapshot.done_reason.clone();
        // created_at 不可序列化，恢复时重置为当前时间
        self.created_at = Instant::now();
    }

    /// 设置任务描述（用于 ERL trajectory 提取）
    pub fn set_task_description(&mut self, desc: String) {
        self.task_description = Some(desc);
    }

    /// 记录步骤到轨迹（供 ERL 提取 heuristics）
    pub fn record_step(&mut self, step_type: &str, content: &str) {
        self.collected_steps.push(TrajectoryStep {
            step_type: step_type.to_string(),
            content: content.to_string(),
        });
    }

    /// 从状态机提取 TaskTrajectory（供 ERL 提取 heuristics）
    ///
    /// 在循环结束时调用，将收集的步骤转换为 TaskTrajectory，
    /// 供 ExperientialReflectiveLearner::extract_heuristics() 使用。
    pub fn extract_trajectory(&self) -> TaskTrajectory {
        let success = matches!(self.done_reason, Some(DoneReason::ModelEndTurn));
        let failure_reason = if success {
            None
        } else {
            self.done_reason.as_ref().map(|r| format!("{:?}", r))
        };

        TaskTrajectory {
            task: self.task_description.clone().unwrap_or_default(),
            steps: self.collected_steps.clone(),
            success,
            failure_reason,
        }
    }

    /// 获取当前状态
    pub fn state(&self) -> super::AgentState {
        self.state
    }

    /// 获取结束原因（仅在 Done/Error 状态时有意义）
    pub fn done_reason(&self) -> Option<&DoneReason> {
        self.done_reason.as_ref()
    }

    /// 获取当前轮次
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// 获取累计消耗的 token
    pub fn tokens_used(&self) -> u64 {
        self.tokens_used
    }

    /// 获取连续失败次数
    pub fn consecutive_failures(&self) -> usize {
        self.consecutive_failures
    }

    /// 获取运行时长
    pub fn elapsed(&self) -> std::time::Duration {
        self.created_at.elapsed()
    }

    /// 判断是否应自动压缩上下文
    ///
    /// 当已使用 token 超过上下文窗口的阈值比例（默认 80%）时，触发压缩。
    /// 这与 CompactionPolicy 的阈值逻辑一致。
    pub fn should_auto_compact(&self, total_tokens: u64, context_window: u64) -> bool {
        if context_window == 0 {
            return false;
        }
        let ratio = total_tokens as f64 / context_window as f64;
        ratio >= AUTO_COMPACT_THRESHOLD_RATIO
    }

    /// 处理事件，转换状态，返回下一步动作
    pub fn transition(&mut self, event: LoopEvent) -> LoopAction {
        // 延迟计算 old_state：仅在 tracing 日志实际输出时才格式化，
        // 避免每次 transition 都分配 String（debug 级别日志通常被过滤掉）
        macro_rules! log_transition {
            ($from:expr, $to:expr, $trigger:expr) => {
                tracing::debug!(
                    target: "ccore::loop",
                    from = %format!("{:?}", $from),
                    to = %$to,
                    trigger = %$trigger,
                    "state transition"
                );
            };
        }

        let old_state = self.state;

        match event {
            LoopEvent::LLMResponse {
                stop_reason,
                token_used,
                tool_calls,
            } => {
                self.tokens_used += token_used;
                self.turn_count += 1;

                match stop_reason.as_str() {
                    "end_turn" | "stop" => {
                        self.state = super::AgentState::Done;
                        self.done_reason = Some(DoneReason::ModelEndTurn);
                        log_transition!(old_state, "Done", "end_turn");
                        LoopAction::EndTurn {
                            reason: DoneReason::ModelEndTurn,
                        }
                    }
                    "tool_use" => {
                        if let Some((id, name, _)) = tool_calls.first() {
                            self.state = super::AgentState::AwaitingApproval;
                            log_transition!(old_state, "AwaitingApproval", format!("tool_use({})", name));
                            LoopAction::WaitForPermission {
                                tool_name: name.clone(),
                                tool_use_id: id.clone(),
                            }
                        } else {
                            // tool_use 但无工具调用，视为 end_turn
                            self.state = super::AgentState::Done;
                            self.done_reason = Some(DoneReason::ModelEndTurn);
                            log_transition!(old_state, "Done", "tool_use_empty");
                            LoopAction::EndTurn {
                                reason: DoneReason::ModelEndTurn,
                            }
                        }
                    }
                    "max_tokens" => {
                        // 达到最大 token，模型想继续但被截断
                        if !tool_calls.is_empty() {
                            if let Some((id, name, _)) = tool_calls.first() {
                                self.state = super::AgentState::AwaitingApproval;
                                log_transition!(old_state, "AwaitingApproval", format!("max_tokens+tool_use({})", name));
                                LoopAction::WaitForPermission {
                                    tool_name: name.clone(),
                                    tool_use_id: id.clone(),
                                }
                            } else {
                                self.state = super::AgentState::Thinking;
                                log_transition!(old_state, "Thinking", "max_tokens");
                                LoopAction::CallLLM
                            }
                        } else {
                            self.state = super::AgentState::Thinking;
                            log_transition!(old_state, "Thinking", "max_tokens");
                            LoopAction::CallLLM
                        }
                    }
                    _ => {
                        // 未知 stop_reason，继续调用 LLM
                        self.state = super::AgentState::Thinking;
                        log_transition!(old_state, "Thinking", format!("unknown_stop_reason({})", stop_reason));
                        LoopAction::CallLLM
                    }
                }
            }

            LoopEvent::ToolExecutionCompleted {
                tool_name: _,
                tool_use_id: _,
                result,
            } => {
                match result {
                    ToolExecutionOutcome::Success => {
                        self.consecutive_failures = 0;
                    }
                    ToolExecutionOutcome::Failure(_) => {
                        self.consecutive_failures += 1;
                        tracing::debug!(
                            target: "ccore::loop",
                            consecutive_failures = self.consecutive_failures,
                            "tool execution failure recorded"
                        );
                    }
                }
                // 工具执行完成后，回到 Thinking 让模型决定下一步
                self.state = super::AgentState::Thinking;
                log_transition!(old_state, "Thinking", "tool_completed");
                LoopAction::CallLLM
            }

            LoopEvent::PermissionDecision {
                tool_name,
                tool_use_id,
                allowed,
            } => {
                if allowed {
                    self.state = super::AgentState::ToolCalling;
                    log_transition!(old_state, "ToolCalling", format!("permission_allowed({})", tool_name));
                    LoopAction::ExecuteTool {
                        tool_name,
                        tool_use_id,
                    }
                } else {
                    self.state = super::AgentState::Done;
                    self.done_reason = Some(DoneReason::PermissionDenied {
                        tool_name: tool_name.clone(),
                    });
                    log_transition!(old_state, "Done", format!("permission_denied({})", tool_name));
                    LoopAction::EndTurn {
                        reason: DoneReason::PermissionDenied { tool_name },
                    }
                }
            }

            LoopEvent::UserCancelled => {
                self.state = super::AgentState::Done;
                self.done_reason = Some(DoneReason::UserCancelled);
                log_transition!(old_state, "Done", "user_cancelled");
                LoopAction::EndTurn {
                    reason: DoneReason::UserCancelled,
                }
            }

            LoopEvent::BudgetExhausted { kind } => {
                let reason = match kind {
                    BudgetKind::Token => DoneReason::BudgetExhausted,
                    BudgetKind::ToolCall => DoneReason::ToolBudgetExhausted,
                };
                self.state = super::AgentState::Done;
                self.done_reason = Some(reason.clone());
                log_transition!(old_state, "Done", format!("budget_exhausted({:?})", kind));
                LoopAction::EndTurn { reason }
            }

            LoopEvent::LoopDetected {
                tool_name,
                repeat_count,
            } => {
                tracing::error!(
                    target: "ccore::loop",
                    consecutive_failures = repeat_count,
                    tool_name = %tool_name,
                    "doom loop detected!"
                );
                let reason = DoneReason::LoopDetected {
                    tool_name: tool_name.clone(),
                    repeat_count,
                };
                self.state = super::AgentState::Done;
                self.done_reason = Some(reason.clone());
                log_transition!(old_state, "Done", format!("loop_detected({})", tool_name));
                LoopAction::EndTurn { reason }
            }

            LoopEvent::ConsecutiveFailures { count } => {
                tracing::error!(
                    target: "ccore::loop",
                    consecutive_failures = count,
                    "doom loop detected!"
                );
                let reason = DoneReason::ConsecutiveFailures { count };
                self.state = super::AgentState::Error;
                self.done_reason = Some(reason.clone());
                log_transition!(old_state, "Error", format!("consecutive_failures({})", count));
                LoopAction::EndTurn { reason }
            }

            LoopEvent::MaxTurnsReached => {
                self.state = super::AgentState::Done;
                self.done_reason = Some(DoneReason::MaxTurnsReached);
                log_transition!(old_state, "Done", "max_turns_reached");
                LoopAction::EndTurn {
                    reason: DoneReason::MaxTurnsReached,
                }
            }
        }
    }

    /// 重置状态机（新 turn 时调用）
    pub fn reset(&mut self) {
        self.state = super::AgentState::Idle;
        self.done_reason = None;
        self.turn_count = 0;
        self.tokens_used = 0;
        self.consecutive_failures = 0;
        self.created_at = Instant::now();
        self.collected_steps.clear();
        self.task_description = None;
    }
}

impl Default for LoopStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_happy_path_idle_to_done() {
        let mut sm = LoopStateMachine::new();
        assert_eq!(sm.state(), super::super::AgentState::Idle);

        // LLM 返回 end_turn
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            token_used: 100,
            tool_calls: vec![],
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ModelEndTurn }));
        assert_eq!(sm.state(), super::super::AgentState::Done);
        assert!(matches!(sm.done_reason(), Some(DoneReason::ModelEndTurn)));
    }

    #[test]
    fn test_tool_use_flow() {
        let mut sm = LoopStateMachine::new();

        // LLM 返回 tool_use
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            token_used: 200,
            tool_calls: vec![("id1".to_string(), "Bash".to_string(), serde_json::json!({"command": "ls"}))],
        });
        assert!(matches!(action, LoopAction::WaitForPermission { .. }));
        assert_eq!(sm.state(), super::super::AgentState::AwaitingApproval);

        // 权限允许
        let action = sm.transition(LoopEvent::PermissionDecision {
            tool_name: "Bash".to_string(),
            tool_use_id: "id1".to_string(),
            allowed: true,
        });
        assert!(matches!(action, LoopAction::ExecuteTool { .. }));
        assert_eq!(sm.state(), super::super::AgentState::ToolCalling);

        // 工具执行成功
        let action = sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "id1".to_string(),
            tool_name: "Bash".to_string(),
            result: ToolExecutionOutcome::Success,
        });
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(sm.state(), super::super::AgentState::Thinking);
        assert_eq!(sm.consecutive_failures(), 0);

        // LLM 返回 end_turn
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            token_used: 150,
            tool_calls: vec![],
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ModelEndTurn }));
        assert_eq!(sm.tokens_used(), 350);
        assert_eq!(sm.turn_count(), 2);
    }

    #[test]
    fn test_consecutive_failures() {
        let mut sm = LoopStateMachine::new();

        // LLM 返回 tool_use
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            token_used: 100,
            tool_calls: vec![("id1".to_string(), "Bash".to_string(), serde_json::json!({}))],
        });
        sm.transition(LoopEvent::PermissionDecision {
            tool_name: "Bash".to_string(),
            tool_use_id: "id1".to_string(),
            allowed: true,
        });

        // 工具执行失败
        sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "id1".to_string(),
            tool_name: "Bash".to_string(),
            result: ToolExecutionOutcome::Failure("error".to_string()),
        });
        assert_eq!(sm.consecutive_failures(), 1);

        // 连续失败触发熔断
        let action = sm.transition(LoopEvent::ConsecutiveFailures { count: 3 });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ConsecutiveFailures { count: 3 } }));
        assert_eq!(sm.state(), super::super::AgentState::Error);
    }

    #[test]
    fn test_permission_denied() {
        let mut sm = LoopStateMachine::new();
        let action = sm.transition(LoopEvent::PermissionDecision {
            tool_name: "Bash".to_string(),
            tool_use_id: "id1".to_string(),
            allowed: false,
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::PermissionDenied { .. } }));
        assert_eq!(sm.state(), super::super::AgentState::Done);
    }

    #[test]
    fn test_loop_detected() {
        let mut sm = LoopStateMachine::new();
        let action = sm.transition(LoopEvent::LoopDetected {
            tool_name: "Read".to_string(),
            repeat_count: 5,
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::LoopDetected { .. } }));
    }

    #[test]
    fn test_loop_detected_from_doom_loop_result() {
        let result = DoomLoopResult {
            detected: true,
            repeated_tool: Some("Read".to_string()),
            repeat_count: 5,
            escape_actions: vec![],
        };
        let event = LoopEvent::from_doom_loop(&result);
        assert!(event.is_some());
        if let LoopEvent::LoopDetected { tool_name, repeat_count } = event.unwrap() {
            assert_eq!(tool_name, "Read");
            assert_eq!(repeat_count, 5);
        } else {
            panic!("Expected LoopDetected event");
        }

        // 未检测到循环
        let no_result = DoomLoopResult {
            detected: false,
            repeated_tool: None,
            repeat_count: 0,
            escape_actions: vec![],
        };
        assert!(LoopEvent::from_doom_loop(&no_result).is_none());
    }

    #[test]
    fn test_budget_exhausted() {
        let mut sm = LoopStateMachine::new();
        let action = sm.transition(LoopEvent::BudgetExhausted {
            kind: BudgetKind::ToolCall,
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ToolBudgetExhausted }));
    }

    #[test]
    fn test_should_auto_compact() {
        let sm = LoopStateMachine::new();

        // 70% 不到阈值，不压缩
        assert!(!sm.should_auto_compact(7000, 10000));

        // 80% 达到阈值，触发压缩
        assert!(sm.should_auto_compact(8000, 10000));

        // 90% 超过阈值，触发压缩
        assert!(sm.should_auto_compact(9000, 10000));

        // context_window 为 0，不触发
        assert!(!sm.should_auto_compact(100, 0));
    }

    #[test]
    fn test_reset() {
        let mut sm = LoopStateMachine::new();
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            token_used: 100,
            tool_calls: vec![],
        });
        assert_eq!(sm.turn_count(), 1);

        sm.reset();
        assert_eq!(sm.state(), super::super::AgentState::Idle);
        assert_eq!(sm.turn_count(), 0);
        assert_eq!(sm.tokens_used(), 0);
        assert!(sm.done_reason().is_none());
    }
}
