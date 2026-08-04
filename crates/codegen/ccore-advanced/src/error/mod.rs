//! ccore 错误分类体系（超越 Claude Code 的细粒度分类）
//!
//! 错误分类原则：
//! 1. 可恢复 vs 不可恢复
//! 2. 网络错误 vs 本地错误
//! 3. 临时性 vs 持久性
//! 4. 用户错误 vs 系统错误

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// ccore 核心错误类型
#[derive(Debug, Error, Clone, Serialize, Deserialize)]
pub enum CcodeError {
    // ===== 网络相关错误（可能可重试）=====

    /// 连接超时（可重试）
    #[error("Connection timeout: {0}")]
    ConnectionTimeout(String),

    /// 连接被拒绝（可重试）
    #[error("Connection refused: {0}")]
    ConnectionRefused(String),

    /// DNS 解析失败（可能可重试）
    #[error("DNS resolution failed: {0}")]
    DnsError(String),

    /// SSL/TLS 错误（通常不可重试）
    #[error("TLS error: {0}")]
    TlsError(String),

    // ===== HTTP 错误 =====

    /// 400 Bad Request（用户错误，不重试）
    #[error("Bad request: {0}")]
    BadRequest(String),

    /// 401 Unauthorized（认证错误，不重试）
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    /// 403 Forbidden（权限错误，不重试）
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// 404 Not Found（资源不存在，不重试）
    #[error("Not found: {0}")]
    NotFound(String),

    /// 429 Too Many Requests（速率限制，可重试）
    #[error("Rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after_secs: Option<u64>,
    },

    /// 500 Internal Server Error（可重试）
    #[error("Server error: {0}")]
    ServerError(String),

    /// 502 Bad Gateway（可重试）
    #[error("Bad gateway: {0}")]
    BadGateway(String),

    /// 503 Service Unavailable（可重试）
    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    /// 504 Gateway Timeout（可重试）
    #[error("Gateway timeout: {0}")]
    GatewayTimeout(String),

    // ===== LLM 相关错误 =====

    /// 上下文窗口溢出（不可重试，需压缩）
    #[error("Context window overflow: {0} tokens")]
    ContextOverflow(u32),

    /// 模型不可用（可重试 fallback）
    #[error("Model unavailable: {0}")]
    ModelUnavailable(String),

    /// 响应格式错误（可重试）
    #[error("Invalid response format: {0}")]
    InvalidResponse(String),

    /// 空响应（可重试）
    #[error("Empty response from LLM")]
    EmptyResponse,

    /// Doom Loop 检测（需换策略）
    #[error("Doom loop detected: {0}")]
    DoomLoopDetected(String),

    // ===== 工具执行错误 =====

    /// 工具不存在
    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    /// 工具参数错误（用户错误，不重试）
    #[error("Invalid tool arguments: {0}")]
    InvalidToolArguments(String),

    /// 工具执行超时（可重试）
    #[error("Tool execution timeout: {0}")]
    ToolTimeout(String),

    /// 工具执行失败（根据内容判断是否可重试）
    #[error("Tool execution failed: {0}")]
    ToolExecutionFailed(String),

    /// 沙箱拦截
    #[error("Sandbox denied: {0}")]
    SandboxDenied(String),

    // ===== 文件系统错误 =====

    /// 文件不存在
    #[error("File not found: {0}")]
    FileNotFound(String),

    /// 权限不足
    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    /// 磁盘空间不足
    #[error("Disk full")]
    DiskFull,

    // ===== Agent 错误 =====

    /// 子 Agent 失败
    #[error("Subagent failed: {0}")]
    SubagentFailed(String),

    /// 子 Agent 超时
    #[error("Subagent timeout: {0}")]
    SubagentTimeout(String),

    /// 最大轮次达到
    #[error("Max turns reached: {0}")]
    MaxTurnsReached(u32),

    /// 预算耗尽
    #[error("Budget exhausted: {kind:?}, used {used}, limit {limit}")]
    BudgetExhausted {
        kind: BudgetKind,
        used: u32,
        limit: u32,
    },

    // ===== 内部错误 =====

    /// 消息序列化失败
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// ZMQ 通信错误
    #[error("ZMQ error: {0}")]
    ZmqError(String),

    /// 内部状态错误
    #[error("Internal error: {0}")]
    InternalError(String),
}

/// 预算类型
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BudgetKind {
    /// Token 预算
    Tokens,
    /// 轮次预算
    Turns,
    /// 工具调用预算
    Tools,
    /// 时间预算
    Time,
}

/// 错误分类器（判断是否可重试）
pub trait ErrorClassifier {
    /// 判断错误是否可重试
    fn is_retryable(&self) -> bool;

    /// 获取建议的退避时间（毫秒）
    fn suggested_backoff_ms(&self) -> Option<u64>;

    /// 是否需要用户干预
    fn requires_user_action(&self) -> bool;
}

impl ErrorClassifier for CcodeError {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            CcodeError::ConnectionTimeout(_)
                | CcodeError::ConnectionRefused(_)
                | CcodeError::DnsError(_)
                | CcodeError::RateLimited { .. }
                | CcodeError::ServerError(_)
                | CcodeError::BadGateway(_)
                | CcodeError::ServiceUnavailable(_)
                | CcodeError::GatewayTimeout(_)
                | CcodeError::ModelUnavailable(_)
                | CcodeError::InvalidResponse(_)
                | CcodeError::EmptyResponse
                | CcodeError::ToolTimeout(_)
                | CcodeError::SubagentTimeout(_)
        )
    }

    fn suggested_backoff_ms(&self) -> Option<u64> {
        match self {
            CcodeError::RateLimited {
                retry_after_secs, ..
            } => Some(retry_after_secs.unwrap_or(60) * 1000),
            CcodeError::ConnectionTimeout(_) => Some(2000),
            CcodeError::ConnectionRefused(_) => Some(5000),
            CcodeError::DnsError(_) => Some(3000),
            CcodeError::ServerError(_) => Some(10000),
            CcodeError::BadGateway(_) => Some(5000),
            CcodeError::ServiceUnavailable(_) => Some(8000),
            CcodeError::GatewayTimeout(_) => Some(5000),
            CcodeError::ToolTimeout(_) => Some(3000),
            _ => None,
        }
    }

    fn requires_user_action(&self) -> bool {
        matches!(
            self,
            CcodeError::Unauthorized(_)
                | CcodeError::Forbidden(_)
                | CcodeError::BadRequest(_)
                | CcodeError::PermissionDenied(_)
                | CcodeError::SandboxDenied(_)
                | CcodeError::DoomLoopDetected(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retryable_errors() {
        // 可重试错误
        assert!(CcodeError::ConnectionTimeout("test".to_string()).is_retryable());
        assert!(CcodeError::ConnectionRefused("test".to_string()).is_retryable());
        assert!(CcodeError::DnsError("test".to_string()).is_retryable());
        assert!(CcodeError::RateLimited {
            message: "test".to_string(),
            retry_after_secs: Some(30)
        }
        .is_retryable());
        assert!(CcodeError::ServerError("test".to_string()).is_retryable());
        assert!(CcodeError::BadGateway("test".to_string()).is_retryable());
        assert!(CcodeError::ServiceUnavailable("test".to_string()).is_retryable());
        assert!(CcodeError::GatewayTimeout("test".to_string()).is_retryable());
        assert!(CcodeError::ModelUnavailable("test".to_string()).is_retryable());
        assert!(CcodeError::InvalidResponse("test".to_string()).is_retryable());
        assert!(CcodeError::EmptyResponse.is_retryable());
        assert!(CcodeError::ToolTimeout("test".to_string()).is_retryable());
        assert!(CcodeError::SubagentTimeout("test".to_string()).is_retryable());
    }

    #[test]
    fn test_non_retryable_errors() {
        // 不可重试错误
        assert!(!CcodeError::BadRequest("test".to_string()).is_retryable());
        assert!(!CcodeError::Unauthorized("test".to_string()).is_retryable());
        assert!(!CcodeError::Forbidden("test".to_string()).is_retryable());
        assert!(!CcodeError::NotFound("test".to_string()).is_retryable());
        assert!(!CcodeError::ContextOverflow(1000).is_retryable());
        assert!(!CcodeError::ToolNotFound("test".to_string()).is_retryable());
        assert!(!CcodeError::FileNotFound("test".to_string()).is_retryable());
        assert!(!CcodeError::PermissionDenied("test".to_string()).is_retryable());
    }

    #[test]
    fn test_suggested_backoff() {
        // RateLimited 有 retry_after
        let error = CcodeError::RateLimited {
            message: "test".to_string(),
            retry_after_secs: Some(30),
        };
        assert_eq!(error.suggested_backoff_ms(), Some(30000));

        // RateLimited 无 retry_after，使用默认值
        let error = CcodeError::RateLimited {
            message: "test".to_string(),
            retry_after_secs: None,
        };
        assert_eq!(error.suggested_backoff_ms(), Some(60000));

        // ConnectionTimeout
        assert_eq!(
            CcodeError::ConnectionTimeout("test".to_string()).suggested_backoff_ms(),
            Some(2000)
        );

        // ServerError
        assert_eq!(
            CcodeError::ServerError("test".to_string()).suggested_backoff_ms(),
            Some(10000)
        );

        // 不可重试错误无建议退避时间
        assert_eq!(
            CcodeError::Unauthorized("test".to_string()).suggested_backoff_ms(),
            None
        );
    }

    #[test]
    fn test_requires_user_action() {
        // 需要用户干预的错误
        assert!(CcodeError::Unauthorized("test".to_string()).requires_user_action());
        assert!(CcodeError::Forbidden("test".to_string()).requires_user_action());
        assert!(CcodeError::BadRequest("test".to_string()).requires_user_action());
        assert!(CcodeError::PermissionDenied("test".to_string()).requires_user_action());
        assert!(CcodeError::SandboxDenied("test".to_string()).requires_user_action());
        assert!(CcodeError::DoomLoopDetected("test".to_string()).requires_user_action());

        // 不需要用户干预的错误
        assert!(!CcodeError::ConnectionTimeout("test".to_string()).requires_user_action());
        assert!(!CcodeError::ServerError("test".to_string()).requires_user_action());
        assert!(!CcodeError::NotFound("test".to_string()).requires_user_action());
    }

    #[test]
    fn test_error_display() {
        // 测试错误消息格式
        let error = CcodeError::ConnectionTimeout("localhost:8080".to_string());
        assert_eq!(error.to_string(), "Connection timeout: localhost:8080");

        let error = CcodeError::ContextOverflow(10000);
        assert_eq!(error.to_string(), "Context window overflow: 10000 tokens");

        let error = CcodeError::BudgetExhausted {
            kind: BudgetKind::Tokens,
            used: 100000,
            limit: 100000,
        };
        assert_eq!(
            error.to_string(),
            "Budget exhausted: Tokens, used 100000, limit 100000"
        );
    }

    #[test]
    fn test_error_serialization() {
        // 测试错误可以序列化和反序列化
        let error = CcodeError::RateLimited {
            message: "Too many requests".to_string(),
            retry_after_secs: Some(60),
        };

        let json = serde_json::to_string(&error).unwrap();
        let restored: CcodeError = serde_json::from_str(&json).unwrap();

        assert!(restored.is_retryable());
        assert_eq!(restored.suggested_backoff_ms(), Some(60000));
    }

    #[test]
    fn test_budget_kind_serialization() {
        // 测试 BudgetKind 序列化
        let kinds = vec![
            BudgetKind::Tokens,
            BudgetKind::Turns,
            BudgetKind::Tools,
            BudgetKind::Time,
        ];

        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            let restored: BudgetKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, restored);
        }
    }

    #[test]
    fn test_budget_exhausted_error() {
        let error = CcodeError::BudgetExhausted {
            kind: BudgetKind::Tokens,
            used: 50000,
            limit: 50000,
        };

        assert!(!error.is_retryable());
        assert!(!error.requires_user_action());
        assert_eq!(error.suggested_backoff_ms(), None);
    }

    #[test]
    fn test_complex_error_scenarios() {
        // 测试复杂场景：网络错误链
        let network_errors = vec![
            CcodeError::ConnectionTimeout("api.example.com".to_string()),
            CcodeError::ConnectionRefused("localhost:8080".to_string()),
            CcodeError::DnsError("unknown.host".to_string()),
        ];

        for error in network_errors {
            assert!(error.is_retryable());
            assert!(!error.requires_user_action());
            assert!(error.suggested_backoff_ms().is_some());
        }

        // 测试复杂场景：HTTP 4xx 错误（不重试）
        let http_4xx_errors = vec![
            CcodeError::BadRequest("Invalid JSON".to_string()),
            CcodeError::Unauthorized("Invalid API key".to_string()),
            CcodeError::Forbidden("Access denied".to_string()),
            CcodeError::NotFound("Resource not found".to_string()),
        ];

        for error in http_4xx_errors {
            assert!(!error.is_retryable());
        }

        // 测试复杂场景：HTTP 5xx 错误（可重试）
        let http_5xx_errors = vec![
            CcodeError::ServerError("Internal error".to_string()),
            CcodeError::BadGateway("Upstream failure".to_string()),
            CcodeError::ServiceUnavailable("Service down".to_string()),
            CcodeError::GatewayTimeout("Upstream timeout".to_string()),
        ];

        for error in http_5xx_errors {
            assert!(error.is_retryable());
            assert!(error.suggested_backoff_ms().is_some());
        }
    }
}