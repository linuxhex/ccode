//! 错误处理集成测试

use ccore::sampler::retry::{classify_sampler_error, RetryDecision, DEFAULT_MAX_RETRIES};
use ccore::agent::loop_state::{LoopStateMachine, LoopEvent, ToolExecutionOutcome, DoneReason};

#[test]
fn test_error_classification_to_retry_decision() {
    // 速率限制 → 应该重试
    let decision = classify_sampler_error("rate limit exceeded", Some(429), 0, DEFAULT_MAX_RETRIES);
    assert!(matches!(decision, RetryDecision::RetryWithBackoff { .. }));

    // 认证错误 → 不应重试
    let decision = classify_sampler_error("invalid api key", Some(401), 0, DEFAULT_MAX_RETRIES);
    assert!(matches!(decision, RetryDecision::Fatal(_)));

    // 超时 → 应该重试
    let decision = classify_sampler_error("connection timed out", None, 0, DEFAULT_MAX_RETRIES);
    assert!(matches!(decision, RetryDecision::Retry { .. }));
}

#[test]
fn test_loop_state_transitions() {
    let mut sm = LoopStateMachine::new();

    // LLM returns tool_use
    let action = sm.transition(LoopEvent::LLMResponse {
        stop_reason: "tool_use".to_string(),
        token_used: 100,
        tool_calls: vec![("id1".to_string(), "Bash".to_string(), serde_json::json!({}))],
    });
    // Should be waiting for permission
    assert!(matches!(action, ccore::agent::loop_state::LoopAction::WaitForPermission { .. }));

    // Permission allowed → execute tool
    let action = sm.transition(LoopEvent::PermissionDecision {
        tool_name: "Bash".to_string(),
        tool_use_id: "id1".to_string(),
        allowed: true,
    });
    assert!(matches!(action, ccore::agent::loop_state::LoopAction::ExecuteTool { .. }));

    // Tool completed (success) → back to thinking
    let action = sm.transition(LoopEvent::ToolExecutionCompleted {
        tool_use_id: "id1".to_string(),
        tool_name: "Bash".to_string(),
        result: ToolExecutionOutcome::Success,
    });
    assert!(matches!(action, ccore::agent::loop_state::LoopAction::CallLLM));
}

#[test]
fn test_doom_loop_detection() {
    let mut sm = LoopStateMachine::new();

    // 连续失败
    for _ in 0..5 {
        sm.transition(LoopEvent::ToolExecutionCompleted {
            tool_use_id: "id1".to_string(),
            tool_name: "Bash".to_string(),
            result: ToolExecutionOutcome::Failure("error".to_string()),
        });
    }

    // 应该检测到连续失败
    assert!(sm.consecutive_failures() >= 5);

    // ConsecutiveFailures 事件触发结束
    let action = sm.transition(LoopEvent::ConsecutiveFailures { count: 5 });
    assert!(matches!(
        action,
        ccore::agent::loop_state::LoopAction::EndTurn {
            reason: DoneReason::ConsecutiveFailures { .. }
        }
    ));
}
