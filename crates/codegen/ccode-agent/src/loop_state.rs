//! Agent 主循环状态机
//!
//! 借鉴 Claude Code 的 queryLoop 深层架构设计，将 Agent 循环建模为状态机：
//! - 每个 turn 结束后，根据 stopReason 决定下一步动作
//! - 错误重试有退避策略（RateLimit 指数退避、ServerError 线性退避）
//! - Token 接近上限时触发 auto-compact
//! - maxTurns 限制防止无限循环
//! - 权限被拒后标记 deny，Agent 可调整策略

use serde::{Deserialize, Serialize};
use std::time::Duration;

// ============================================================================
// 核心状态机
// ============================================================================

/// Agent 主循环状态机
///
/// 管理整个 Agent 循环的生命周期：接收事件 → 状态转换 → 输出动作。
/// 所有决策逻辑集中在 `transition` 方法中，调用方只需驱动事件流。
pub struct AgentLoopStateMachine {
    /// 当前状态
    state: LoopState,
    /// 已执行轮次
    turn_count: u32,
    /// 最大轮次
    max_turns: u32,
    /// Token 使用情况
    token_usage: TokenUsage,
    /// auto-compact 触发阈值（百分比，0–100）
    auto_compact_threshold_percent: u8,
    /// 错误重试计数器
    retry_counts: ErrorRetryCounts,
    /// 被拒绝的工具列表（deny recovery 用）
    denied_tools: Vec<DeniedToolCall>,
}

impl AgentLoopStateMachine {
    /// 创建状态机实例
    ///
    /// - `max_turns`: 最大轮次，超过后强制结束
    /// - `token_budget`: Token 总预算，用于 auto-compact 判定
    /// - `auto_compact_threshold_percent`: Token 使用率超过此阈值时触发压缩
    pub fn new(max_turns: u32, token_budget: u64, auto_compact_threshold_percent: u8) -> Self {
        Self {
            state: LoopState::WaitingForInput,
            turn_count: 0,
            max_turns,
            token_usage: TokenUsage {
                used: 0,
                budget: token_budget,
            },
            auto_compact_threshold_percent,
            retry_counts: ErrorRetryCounts::default(),
            denied_tools: Vec::new(),
        }
    }

    /// 状态机核心：接收事件，执行状态转换，返回 Agent 应执行的动作
    ///
    /// 所有决策逻辑都在这里——调用方只需根据返回的 `LoopAction` 执行操作，
    /// 然后将执行结果作为新的 `LoopEvent` 喂回来，形成驱动循环。
    pub fn transition(&mut self, event: LoopEvent) -> LoopAction {
        match event {
            LoopEvent::UserInputReceived => {
                self.state = LoopState::CallingLLM;
                self.turn_count += 1;
                LoopAction::CallLLM
            }

            LoopEvent::LLMResponse {
                stop_reason,
                tool_calls,
                token_used,
            } => {
                self.token_usage.used += token_used;

                // 优先检查 auto-compact：Token 使用率超阈值时压缩上下文
                if self
                    .token_usage
                    .should_compact(self.auto_compact_threshold_percent)
                {
                    self.state = LoopState::Compacting;
                    return LoopAction::CompactContext;
                }

                // 检查 maxTurns：防止无限循环
                if self.turn_count >= self.max_turns {
                    self.state = LoopState::Finished {
                        reason: FinishReason::MaxTurnsReached,
                    };
                    return LoopAction::Finish {
                        reason: FinishReason::MaxTurnsReached,
                    };
                }

                // 根据 LLM 返回的 stopReason 决定下一步
                match stop_reason.as_str() {
                    // LLM 认为任务完成，回到等待输入
                    "end_turn" => {
                        self.state = LoopState::WaitingForInput;
                        LoopAction::WaitForInput
                    }
                    // LLM 请求执行工具
                    "tool_use" => {
                        if let Some((id, name, input)) = tool_calls.first() {
                            self.state = LoopState::ExecutingTool {
                                tool_name: name.clone(),
                                tool_use_id: id.clone(),
                            };
                            LoopAction::ExecuteTool {
                                tool_name: name.clone(),
                                tool_use_id: id.clone(),
                                input: input.clone(),
                            }
                        } else {
                            // tool_use 但无工具调用——退回 LLM 重新生成
                            self.state = LoopState::CallingLLM;
                            LoopAction::CallLLM
                        }
                    }
                    // 输出被截断，压缩后继续
                    "max_tokens" => {
                        self.state = LoopState::Compacting;
                        LoopAction::CompactContext
                    }
                    // 遇到停止序列，循环正常结束
                    "stop_sequence" => {
                        self.state = LoopState::Finished {
                            reason: FinishReason::EndTurn,
                        };
                        LoopAction::Finish {
                            reason: FinishReason::EndTurn,
                        }
                    }
                    // 未知 stopReason，安全地回到等待输入
                    _ => {
                        self.state = LoopState::WaitingForInput;
                        LoopAction::WaitForInput
                    }
                }
            }

            // 工具执行完成，将结果返回 LLM 继续推理
            LoopEvent::ToolExecutionCompleted { .. } => {
                self.state = LoopState::ToolResultReady;
                self.turn_count += 1;
                LoopAction::CallLLM
            }

            // 工具执行失败，仍然让 LLM 知道（通过错误结果），以便调整策略
            LoopEvent::ToolExecutionFailed { .. } => {
                self.state = LoopState::ToolResultReady;
                self.turn_count += 1;
                LoopAction::CallLLM
            }

            // 权限被拒：记录拒绝信息，Agent 可调整策略（比如换工具或修改参数）
            LoopEvent::PermissionDenied { tool_name, reason } => {
                self.denied_tools.push(DeniedToolCall {
                    tool_name: tool_name.clone(),
                    reason: reason.clone(),
                    denied_at: chrono::Utc::now().to_rfc3339(),
                });
                self.state = LoopState::PermissionDenied {
                    tool_name: tool_name.clone(),
                    reason: reason.clone(),
                };
                LoopAction::AdjustStrategyAfterDeny {
                    denied_tool: tool_name,
                    reason,
                }
            }

            // API 错误：根据错误类型决定重试策略
            LoopEvent::APIError {
                error_kind,
                message,
            } => {
                let max_retries = error_kind.max_retries();
                let current = self.get_retry_count(&error_kind);

                if current >= max_retries {
                    // 超出最大重试次数，不可恢复
                    self.state = LoopState::Finished {
                        reason: FinishReason::UnrecoverableError(message.clone()),
                    };
                    return LoopAction::Finish {
                        reason: FinishReason::UnrecoverableError(message),
                    };
                }

                // 增加重试计数，按退避策略等待后重试
                self.increment_retry_count(&error_kind);
                let backoff = error_kind.backoff_duration(current);
                self.state = LoopState::ErrorRetrying {
                    error_kind: error_kind.clone(),
                    attempt: current + 1,
                };
                LoopAction::RetryAfterBackoff { duration: backoff }
            }

            // 上下文压缩完成，继续调用 LLM
            LoopEvent::CompactCompleted => {
                self.state = LoopState::CallingLLM;
                LoopAction::CallLLM
            }

            // 用户主动中断
            LoopEvent::UserInterrupt => {
                self.state = LoopState::Finished {
                    reason: FinishReason::UserInterrupt,
                };
                LoopAction::Finish {
                    reason: FinishReason::UserInterrupt,
                }
            }
        }
    }

    /// 获取指定错误类型的当前重试次数
    fn get_retry_count(&self, kind: &ErrorKind) -> u32 {
        match kind {
            ErrorKind::RateLimit => self.retry_counts.rate_limit,
            ErrorKind::ServerError => self.retry_counts.server_error,
            ErrorKind::MaxOutputTokens => self.retry_counts.max_output_tokens,
            ErrorKind::Unknown => self.retry_counts.unknown,
            // 这两类不会重试，计数始终为 0
            ErrorKind::AuthenticationFailed | ErrorKind::InvalidRequest => 0,
        }
    }

    /// 递增指定错误类型的重试计数
    fn increment_retry_count(&mut self, kind: &ErrorKind) {
        match kind {
            ErrorKind::RateLimit => self.retry_counts.rate_limit += 1,
            ErrorKind::ServerError => self.retry_counts.server_error += 1,
            ErrorKind::MaxOutputTokens => self.retry_counts.max_output_tokens += 1,
            ErrorKind::Unknown => self.retry_counts.unknown += 1,
            ErrorKind::AuthenticationFailed | ErrorKind::InvalidRequest => {
                // 不可重试的错误，不递增计数
            }
        }
    }

    /// 检查指定工具是否曾被拒绝（用于 deny recovery 判断）
    pub fn is_denied(&self, tool_name: &str) -> bool {
        self.denied_tools
            .last()
            .is_some_and(|d| d.tool_name == tool_name)
    }

    /// 获取最近的 n 条拒绝记录（用于 Agent 调整策略时参考）
    pub fn get_recent_denials(&self, last_n: usize) -> &[DeniedToolCall] {
        let start = self.denied_tools.len().saturating_sub(last_n);
        &self.denied_tools[start..]
    }

    /// 获取当前状态
    pub fn state(&self) -> &LoopState {
        &self.state
    }

    /// 获取已执行轮次
    pub fn turn_count(&self) -> u32 {
        self.turn_count
    }

    /// 获取 Token 使用情况
    pub fn token_usage(&self) -> &TokenUsage {
        &self.token_usage
    }
}

// ============================================================================
// 循环状态
// ============================================================================

/// 循环状态
///
/// 表示 Agent 主循环在某一时刻所处的阶段。
/// 状态之间的转换由 `AgentLoopStateMachine::transition` 驱动。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopState {
    /// 等待用户输入
    WaitingForInput,
    /// 正在调用 LLM
    CallingLLM,
    /// LLM 返回了工具调用，需要执行
    ExecutingTool {
        tool_name: String,
        tool_use_id: String,
    },
    /// 工具执行完成，需要继续 LLM 调用
    ToolResultReady,
    /// LLM 请求压缩上下文（Token 接近上限或输出被截断）
    Compacting,
    /// 权限被拒绝，Agent 需要调整策略
    PermissionDenied { tool_name: String, reason: String },
    /// API 错误，需要按退避策略重试
    ErrorRetrying { error_kind: ErrorKind, attempt: u32 },
    /// 循环结束
    Finished { reason: FinishReason },
}

// ============================================================================
// 结束原因
// ============================================================================

/// 结束原因
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    /// LLM 返回 end_turn，Agent 认为任务完成
    EndTurn,
    /// 达到最大轮次
    MaxTurnsReached,
    /// 用户中断
    UserInterrupt,
    /// 不可恢复的错误
    UnrecoverableError(String),
}

// ============================================================================
// 错误类型及重试策略
// ============================================================================

/// 错误类型及对应重试策略
///
/// 不同 API 错误的重试策略不同：
/// - RateLimit：指数退避，最多重试 5 次
/// - ServerError：线性退避，最多重试 3 次
/// - MaxOutputTokens：短暂等待后压缩重试，最多 2 次
/// - AuthenticationFailed/InvalidRequest：不重试
/// - Unknown：短暂重试 1 次
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// 速率限制 — 指数退避重试
    RateLimit,
    /// 认证失败 — 不重试
    AuthenticationFailed,
    /// 服务端错误 — 线性退避重试
    ServerError,
    /// 请求无效 — 不重试
    InvalidRequest,
    /// 输出超长 — 压缩后重试
    MaxOutputTokens,
    /// 未知错误 — 短暂重试
    Unknown,
}

impl ErrorKind {
    /// 获取该错误类型的最大重试次数
    pub fn max_retries(&self) -> u32 {
        match self {
            Self::RateLimit => 5,
            Self::ServerError => 3,
            Self::MaxOutputTokens => 2,
            Self::Unknown => 1,
            Self::AuthenticationFailed | Self::InvalidRequest => 0,
        }
    }

    /// 获取该错误类型在第 N 次重试时的退避时间
    pub fn backoff_duration(&self, attempt: u32) -> Duration {
        match self {
            // 指数退避：1s, 2s, 4s, 8s, 16s
            Self::RateLimit => Duration::from_millis(1000 * 2u64.pow(attempt)),
            // 线性退避：1s, 2s, 3s
            Self::ServerError => Duration::from_secs(attempt as u64 + 1),
            // 极短等待，之后会触发 compact
            Self::MaxOutputTokens => Duration::from_millis(100),
            // 固定 1s
            Self::Unknown => Duration::from_secs(1),
            // 不可重试，退避时间为 0
            Self::AuthenticationFailed | Self::InvalidRequest => Duration::ZERO,
        }
    }
}

// ============================================================================
// Token 使用情况
// ============================================================================

/// Token 使用情况
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// 已使用的 token 数
    pub used: u64,
    /// 总 token 预算
    pub budget: u64,
}

impl TokenUsage {
    /// 使用率百分比（0–100）
    pub fn usage_percent(&self) -> u8 {
        if self.budget == 0 {
            return 0;
        }
        ((self.used * 100) / self.budget) as u8
    }

    /// 是否需要压缩上下文
    pub fn should_compact(&self, threshold_percent: u8) -> bool {
        self.usage_percent() >= threshold_percent
    }
}

// ============================================================================
// 被拒绝的工具调用记录
// ============================================================================

/// 被拒绝的工具调用记录
///
/// 用于 deny recovery：当权限被拒后，Agent 可以查阅历史拒绝记录，
/// 调整策略（比如换工具、修改参数、向用户说明等）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeniedToolCall {
    /// 被拒绝的工具名
    pub tool_name: String,
    /// 拒绝原因
    pub reason: String,
    /// 拒绝时间（RFC 3339 格式）
    pub denied_at: String,
}

// ============================================================================
// 错误重试计数器
// ============================================================================

/// 错误重试计数器
///
/// 按错误类型分别记录重试次数，用于判断是否超出最大重试次数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ErrorRetryCounts {
    /// RateLimit 错误重试次数
    pub rate_limit: u32,
    /// ServerError 错误重试次数
    pub server_error: u32,
    /// MaxOutputTokens 错误重试次数
    pub max_output_tokens: u32,
    /// Unknown 错误重试次数
    pub unknown: u32,
}

// ============================================================================
// 事件与动作
// ============================================================================

/// 循环事件
///
/// Agent 主循环中可能发生的事件。调用方将外部事件（LLM 响应、工具执行结果等）
/// 封装为此枚举喂给状态机，状态机据此转换状态并输出动作。
#[derive(Debug, Clone)]
pub enum LoopEvent {
    /// 用户输入已接收，开始新一轮
    UserInputReceived,
    /// LLM 返回响应
    LLMResponse {
        /// LLM 返回的停止原因（end_turn / tool_use / max_tokens / stop_sequence）
        stop_reason: String,
        /// LLM 请求调用的工具列表：(id, name, input) 三元组
        tool_calls: Vec<(String, String, serde_json::Value)>,
        /// 本次响应消耗的 token 数
        token_used: u64,
    },
    /// 工具执行完成
    ToolExecutionCompleted {
        /// 完成的工具调用 ID
        tool_use_id: String,
        /// 工具执行结果内容
        result: String,
    },
    /// 工具执行失败
    ToolExecutionFailed {
        /// 失败的工具调用 ID
        tool_use_id: String,
        /// 失败原因
        error: String,
    },
    /// 权限被拒绝（用户或系统拒绝了工具执行）
    PermissionDenied {
        /// 被拒绝的工具名
        tool_name: String,
        /// 拒绝原因
        reason: String,
    },
    /// API 错误
    APIError {
        /// 错误类型
        error_kind: ErrorKind,
        /// 错误信息
        message: String,
    },
    /// 上下文压缩完成
    CompactCompleted,
    /// 用户主动中断
    UserInterrupt,
}

/// 循环动作
///
/// 状态机输出的动作，调用方据此执行具体操作（调 LLM、执行工具、等待等）。
#[derive(Debug, Clone)]
pub enum LoopAction {
    /// 调用 LLM
    CallLLM,
    /// 执行工具
    ExecuteTool {
        /// 工具名
        tool_name: String,
        /// 工具调用 ID
        tool_use_id: String,
        /// 工具输入参数
        input: serde_json::Value,
    },
    /// 压缩上下文
    CompactContext,
    /// 等待用户输入
    WaitForInput,
    /// 退避后重试
    RetryAfterBackoff {
        /// 退避等待时间
        duration: Duration,
    },
    /// 循环结束
    Finish {
        /// 结束原因
        reason: FinishReason,
    },
    /// 权限被拒后调整策略
    AdjustStrategyAfterDeny {
        /// 被拒绝的工具名
        denied_tool: String,
        /// 拒绝原因
        reason: String,
    },
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_machine() {
        let sm = AgentLoopStateMachine::new(10, 100_000, 80);
        assert_eq!(*sm.state(), LoopState::WaitingForInput);
        assert_eq!(sm.turn_count(), 0);
        assert_eq!(sm.token_usage().budget, 100_000);
    }

    #[test]
    fn test_user_input_to_calling_llm() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 80);
        let action = sm.transition(LoopEvent::UserInputReceived);
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(*sm.state(), LoopState::CallingLLM);
        assert_eq!(sm.turn_count(), 1);
    }

    #[test]
    fn test_llm_end_turn() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 80);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            tool_calls: vec![],
            token_used: 500,
        });
        assert!(matches!(action, LoopAction::WaitForInput));
        assert_eq!(*sm.state(), LoopState::WaitingForInput);
        assert_eq!(sm.token_usage().used, 500);
    }

    #[test]
    fn test_llm_tool_use() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 80);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![(
                "tc_1".to_string(),
                "read_file".to_string(),
                serde_json::json!({"path": "/tmp/test.rs"}),
            )],
            token_used: 300,
        });
        match action {
            LoopAction::ExecuteTool { tool_name, .. } => {
                assert_eq!(tool_name, "read_file");
            }
            other => panic!("期望 ExecuteTool，得到 {:?}", other),
        }
        assert_eq!(
            *sm.state(),
            LoopState::ExecutingTool {
                tool_name: "read_file".to_string(),
                tool_use_id: "tc_1".to_string(),
            }
        );
    }

    #[test]
    fn test_tool_result_to_call_llm() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 80);
        sm.transition(LoopEvent::UserInputReceived);
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![(
                "tc_1".to_string(),
                "read_file".to_string(),
                serde_json::json!({}),
            )],
            token_used: 100,
        });
        let action = sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "tc_1".to_string(),
            result: "file content".to_string(),
        });
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(*sm.state(), LoopState::ToolResultReady);
    }

    #[test]
    fn test_auto_compact_triggered() {
        // 阈值 50%，budget 1000，已用 600 超阈值
        let mut sm = AgentLoopStateMachine::new(10, 1000, 50);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "end_turn".to_string(),
            tool_calls: vec![],
            token_used: 600,
        });
        assert!(matches!(action, LoopAction::CompactContext));
        assert_eq!(*sm.state(), LoopState::Compacting);
    }

    #[test]
    fn test_max_tokens_triggers_compact() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "max_tokens".to_string(),
            tool_calls: vec![],
            token_used: 100,
        });
        assert!(matches!(action, LoopAction::CompactContext));
    }

    #[test]
    fn test_compact_completed_continues() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 80);
        sm.transition(LoopEvent::UserInputReceived);
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "max_tokens".to_string(),
            tool_calls: vec![],
            token_used: 100,
        });
        let action = sm.transition(LoopEvent::CompactCompleted);
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(*sm.state(), LoopState::CallingLLM);
    }

    #[test]
    fn test_max_turns_reached() {
        let mut sm = AgentLoopStateMachine::new(2, 100_000, 90);
        // turn 1
        sm.transition(LoopEvent::UserInputReceived);
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![(
                "tc_1".to_string(),
                "read_file".to_string(),
                serde_json::json!({}),
            )],
            token_used: 100,
        });
        // turn 2（达到 max_turns=2）
        let action = sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "tc_1".to_string(),
            result: "ok".to_string(),
        });
        assert!(matches!(action, LoopAction::CallLLM));
        // 下一次 LLM 响应将触发 maxTurns 检查
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![(
                "tc_2".to_string(),
                "write_file".to_string(),
                serde_json::json!({}),
            )],
            token_used: 100,
        });
        match action {
            LoopAction::Finish {
                reason: FinishReason::MaxTurnsReached,
            } => {}
            other => panic!("期望 Finish(MaxTurnsReached)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_stop_sequence_finishes() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "stop_sequence".to_string(),
            tool_calls: vec![],
            token_used: 100,
        });
        match action {
            LoopAction::Finish {
                reason: FinishReason::EndTurn,
            } => {}
            other => panic!("期望 Finish(EndTurn)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_user_interrupt() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        let action = sm.transition(LoopEvent::UserInterrupt);
        match action {
            LoopAction::Finish {
                reason: FinishReason::UserInterrupt,
            } => {}
            other => panic!("期望 Finish(UserInterrupt)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_permission_denied() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        let action = sm.transition(LoopEvent::PermissionDenied {
            tool_name: "run_terminal_command".to_string(),
            reason: "用户拒绝执行".to_string(),
        });
        match action {
            LoopAction::AdjustStrategyAfterDeny {
                denied_tool,
                reason,
            } => {
                assert_eq!(denied_tool, "run_terminal_command");
                assert_eq!(reason, "用户拒绝执行");
            }
            other => panic!("期望 AdjustStrategyAfterDeny，得到 {:?}", other),
        }
        assert!(sm.is_denied("run_terminal_command"));
        assert!(!sm.is_denied("read_file"));
    }

    #[test]
    fn test_get_recent_denials() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::PermissionDenied {
            tool_name: "tool_a".to_string(),
            reason: "拒绝A".to_string(),
        });
        sm.transition(LoopEvent::PermissionDenied {
            tool_name: "tool_b".to_string(),
            reason: "拒绝B".to_string(),
        });
        sm.transition(LoopEvent::PermissionDenied {
            tool_name: "tool_c".to_string(),
            reason: "拒绝C".to_string(),
        });
        let recent = sm.get_recent_denials(2);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].tool_name, "tool_b");
        assert_eq!(recent[1].tool_name, "tool_c");
    }

    #[test]
    fn test_api_error_retry_rate_limit() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        // 第 1 次重试
        let action = sm.transition(LoopEvent::APIError {
            error_kind: ErrorKind::RateLimit,
            message: "rate limited".to_string(),
        });
        match action {
            LoopAction::RetryAfterBackoff { duration } => {
                assert_eq!(duration, Duration::from_millis(1000)); // 2^0 * 1000
            }
            other => panic!("期望 RetryAfterBackoff，得到 {:?}", other),
        }
        assert_eq!(
            *sm.state(),
            LoopState::ErrorRetrying {
                error_kind: ErrorKind::RateLimit,
                attempt: 1,
            }
        );

        // 第 2 次重试
        let action = sm.transition(LoopEvent::APIError {
            error_kind: ErrorKind::RateLimit,
            message: "rate limited".to_string(),
        });
        match action {
            LoopAction::RetryAfterBackoff { duration } => {
                assert_eq!(duration, Duration::from_millis(2000)); // 2^1 * 1000
            }
            other => panic!("期望 RetryAfterBackoff，得到 {:?}", other),
        }
    }

    #[test]
    fn test_api_error_no_retry_auth() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        let action = sm.transition(LoopEvent::APIError {
            error_kind: ErrorKind::AuthenticationFailed,
            message: "无效 API Key".to_string(),
        });
        match action {
            LoopAction::Finish {
                reason: FinishReason::UnrecoverableError(msg),
            } => {
                assert_eq!(msg, "无效 API Key");
            }
            other => panic!("期望 Finish(UnrecoverableError)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_api_error_exhaust_retries() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        // Unknown 最多重试 1 次，第 2 次 should exhaust
        sm.transition(LoopEvent::APIError {
            error_kind: ErrorKind::Unknown,
            message: "unknown error".to_string(),
        });
        let action = sm.transition(LoopEvent::APIError {
            error_kind: ErrorKind::Unknown,
            message: "still unknown".to_string(),
        });
        match action {
            LoopAction::Finish {
                reason: FinishReason::UnrecoverableError(msg),
            } => {
                assert_eq!(msg, "still unknown");
            }
            other => panic!("期望 Finish(UnrecoverableError)，得到 {:?}", other),
        }
    }

    #[test]
    fn test_error_kind_backoff() {
        // RateLimit 指数退避
        assert_eq!(
            ErrorKind::RateLimit.backoff_duration(0),
            Duration::from_millis(1000)
        );
        assert_eq!(
            ErrorKind::RateLimit.backoff_duration(1),
            Duration::from_millis(2000)
        );
        assert_eq!(
            ErrorKind::RateLimit.backoff_duration(4),
            Duration::from_millis(16000)
        );

        // ServerError 线性退避
        assert_eq!(
            ErrorKind::ServerError.backoff_duration(0),
            Duration::from_secs(1)
        );
        assert_eq!(
            ErrorKind::ServerError.backoff_duration(2),
            Duration::from_secs(3)
        );

        // 不可重试
        assert_eq!(
            ErrorKind::AuthenticationFailed.backoff_duration(0),
            Duration::ZERO
        );
        assert_eq!(
            ErrorKind::InvalidRequest.backoff_duration(0),
            Duration::ZERO
        );
    }

    #[test]
    fn test_token_usage() {
        let usage = TokenUsage {
            used: 7500,
            budget: 10000,
        };
        assert_eq!(usage.usage_percent(), 75);
        assert!(usage.should_compact(70));
        assert!(!usage.should_compact(80));

        // budget 为 0 时安全返回 0
        let zero = TokenUsage {
            used: 100,
            budget: 0,
        };
        assert_eq!(zero.usage_percent(), 0);
    }

    #[test]
    fn test_tool_execution_failed_continues() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::UserInputReceived);
        sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![(
                "tc_1".to_string(),
                "bad_tool".to_string(),
                serde_json::json!({}),
            )],
            token_used: 100,
        });
        let action = sm.transition(LoopEvent::ToolExecutionFailed {
            tool_use_id: "tc_1".to_string(),
            error: "工具崩溃".to_string(),
        });
        // 工具执行失败仍然继续调 LLM，让模型知道失败并调整
        assert!(matches!(action, LoopAction::CallLLM));
        assert_eq!(*sm.state(), LoopState::ToolResultReady);
    }

    #[test]
    fn test_tool_use_with_empty_calls() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::UserInputReceived);
        // stop_reason 是 tool_use 但无实际工具调用——退回 LLM
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "tool_use".to_string(),
            tool_calls: vec![],
            token_used: 100,
        });
        assert!(matches!(action, LoopAction::CallLLM));
    }

    #[test]
    fn test_unknown_stop_reason() {
        let mut sm = AgentLoopStateMachine::new(10, 100_000, 90);
        sm.transition(LoopEvent::UserInputReceived);
        let action = sm.transition(LoopEvent::LLMResponse {
            stop_reason: "something_unknown".to_string(),
            tool_calls: vec![],
            token_used: 100,
        });
        assert!(matches!(action, LoopAction::WaitForInput));
    }
}
