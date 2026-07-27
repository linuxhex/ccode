//! Agent 主循环状态机（借鉴 Claude Code queryLoop 设计）
//!
//! 将主循环从隐式 `loop {}` 升级为显式状态机驱动，使状态变迁可观测、可中断、可恢复。
//! 状态机不替代 `process_conversation_turn_with_recovery`，而是在外层循环上包装，
//! 追踪"当前处于循环的哪个阶段"。

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 主循环状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopState {
    /// 初始状态，等待第一次 LLM 调用
    Idle,
    /// 正在调用 LLM
    CallingLLM,
    /// 正在执行工具
    ExecutingTools,
    /// 等待用户权限确认
    WaitingPermission {
        tool_name: String,
        tool_use_id: String,
    },
    /// 循环结束
    Done {
        reason: DoneReason,
    },
}

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
    /// 循环检测触发
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

/// 预算类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetKind {
    Token,
    ToolCall,
}

/// 工具执行结果
#[derive(Debug, Clone)]
pub enum ToolExecutionOutcome {
    Success,
    Failure(String),
}

/// 状态机输出的动作——告诉主循环下一步做什么
#[derive(Debug, Clone)]
pub enum LoopAction {
    /// 调用 LLM（主循环执行 process_conversation_turn_with_recovery）
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

/// 主循环状态机
#[derive(Debug)]
pub struct LoopStateMachine {
    /// 当前状态
    state: LoopState,
    /// 当前轮次（从 0 开始，每次 CallLLM 后递增）
    turn_count: u32,
    /// 累计消耗的 token
    tokens_used: u64,
    /// 连续工具失败次数
    consecutive_failures: usize,
    /// 创建时间
    created_at: Instant,
}

impl LoopStateMachine {
    /// 创建新的状态机，初始状态为 Idle
    pub fn new() -> Self {
        Self {
            state: LoopState::Idle,
            turn_count: 0,
            tokens_used: 0,
            consecutive_failures: 0,
            created_at: Instant::now(),
        }
    }

    /// 获取当前状态
    pub fn state(&self) -> &LoopState {
        &self.state
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

    /// 处理事件，转换状态，返回下一步动作
    pub fn transition(&mut self, event: LoopEvent) -> LoopAction {
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
                        self.state = LoopState::Done {
                            reason: DoneReason::ModelEndTurn,
                        };
                        LoopAction::EndTurn {
                            reason: DoneReason::ModelEndTurn,
                        }
                    }
                    "tool_use" => {
                        if let Some((id, name, _)) = tool_calls.first() {
                            self.state = LoopState::ExecutingTools;
                            LoopAction::ExecuteTool {
                                tool_name: name.clone(),
                                tool_use_id: id.clone(),
                            }
                        } else {
                            // tool_use 但无工具调用，视为 end_turn
                            self.state = LoopState::Done {
                                reason: DoneReason::ModelEndTurn,
                            };
                            LoopAction::EndTurn {
                                reason: DoneReason::ModelEndTurn,
                            }
                        }
                    }
                    "max_tokens" => {
                        // 达到最大 token，模型想继续但被截断
                        // 如果有工具调用，执行工具；否则继续调用 LLM
                        if !tool_calls.is_empty() {
                            if let Some((id, name, _)) = tool_calls.first() {
                                self.state = LoopState::ExecutingTools;
                                LoopAction::ExecuteTool {
                                    tool_name: name.clone(),
                                    tool_use_id: id.clone(),
                                }
                            } else {
                                self.state = LoopState::CallingLLM;
                                LoopAction::CallLLM
                            }
                        } else {
                            self.state = LoopState::CallingLLM;
                            LoopAction::CallLLM
                        }
                    }
                    _ => {
                        // 未知 stop_reason，继续调用 LLM
                        self.state = LoopState::CallingLLM;
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
                    }
                }
                // 工具执行完成后，回到 CallingLLM 让模型决定下一步
                self.state = LoopState::CallingLLM;
                LoopAction::CallLLM
            }

            LoopEvent::PermissionDecision {
                tool_name,
                tool_use_id,
                allowed,
            } => {
                if allowed {
                    self.state = LoopState::ExecutingTools;
                    LoopAction::ExecuteTool {
                        tool_name,
                        tool_use_id,
                    }
                } else {
                    self.state = LoopState::Done {
                        reason: DoneReason::PermissionDenied {
                            tool_name: tool_name.clone(),
                        },
                    };
                    LoopAction::EndTurn {
                        reason: DoneReason::PermissionDenied { tool_name },
                    }
                }
            }

            LoopEvent::UserCancelled => {
                self.state = LoopState::Done {
                    reason: DoneReason::UserCancelled,
                };
                LoopAction::EndTurn {
                    reason: DoneReason::UserCancelled,
                }
            }

            LoopEvent::BudgetExhausted { kind } => {
                let reason = match kind {
                    BudgetKind::Token => DoneReason::BudgetExhausted,
                    BudgetKind::ToolCall => DoneReason::ToolBudgetExhausted,
                };
                self.state = LoopState::Done { reason: reason.clone() };
                LoopAction::EndTurn { reason }
            }

            LoopEvent::LoopDetected {
                tool_name,
                repeat_count,
            } => {
                let reason = DoneReason::LoopDetected {
                    tool_name: tool_name.clone(),
                    repeat_count,
                };
                self.state = LoopState::Done { reason: reason.clone() };
                LoopAction::EndTurn { reason }
            }

            LoopEvent::ConsecutiveFailures { count } => {
                let reason = DoneReason::ConsecutiveFailures { count };
                self.state = LoopState::Done { reason: reason.clone() };
                LoopAction::EndTurn { reason }
            }

            LoopEvent::MaxTurnsReached => {
                self.state = LoopState::Done {
                    reason: DoneReason::MaxTurnsReached,
                };
                LoopAction::EndTurn {
                    reason: DoneReason::MaxTurnsReached,
                }
            }
        }
    }

    /// 重置状态机（新 turn 时调用）
    pub fn reset(&mut self) {
        self.state = LoopState::Idle;
        self.turn_count = 0;
        self.tokens_used = 0;
        self.consecutive_failures = 0;
        self.created_at = Instant::now();
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
        assert_eq!(*sm.state(), LoopState::Idle);

        // LLM 返回 end_turn
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            token_used: 100,
            tool_calls: vec![],
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ModelEndTurn }));
        assert_eq!(*sm.state(), LoopState::Done { reason: DoneReason::ModelEndTurn });
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
        assert!(matches!(action, LoopAction::ExecuteTool { .. }));
        assert_eq!(*sm.state(), LoopState::ExecutingTools);

        // 工具执行成功
        let action = sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "id1".to_string(),
            tool_name: "Bash".to_string(),
            result: ToolExecutionOutcome::Success,
        });
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(*sm.state(), LoopState::CallingLLM);
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
    fn test_budget_exhausted() {
        let mut sm = LoopStateMachine::new();
        let action = sm.transition(LoopEvent::BudgetExhausted {
            kind: BudgetKind::ToolCall,
        });
        assert!(matches!(action, LoopAction::EndTurn { reason: DoneReason::ToolBudgetExhausted }));
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
        assert_eq!(*sm.state(), LoopState::Idle);
        assert_eq!(sm.turn_count(), 0);
        assert_eq!(sm.tokens_used(), 0);
    }
}
