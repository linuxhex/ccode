//! Hook 桥接 — 将 ccode-hooks dispatcher 桥接到 ccore 的工具执行路径
//!
//! ccore 不直接依赖 ccode-hooks，而是通过 trait object 解耦。
//! ccode-shell 负责创建 HookDispatcherAdapter 并注入到 ToolNode。

use async_trait::async_trait;
use serde_json::Value;

/// Hook 决策结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    /// 允许工具执行
    Allow,
    /// 拒绝工具执行
    Deny(String),
    /// 重写工具输入
    Rewrite { updated_input: Value, additional_context: Option<String> },
}

/// 工具执行上下文（供 Hook 判断）
#[derive(Debug, Clone)]
pub struct ToolCallContext {
    /// 工具名称
    pub tool_name: String,
    /// 工具输入参数
    pub input: Value,
    /// 调用者 Agent ID
    pub agent_id: String,
    /// 工作目录
    pub working_dir: String,
}

/// Hook 桥接 trait — ccore 通过此 trait 调用 Hook 系统
///
/// 实现者（ccode-shell 的 HookDispatcherAdapter）负责：
/// 1. 将 ToolCallContext 转换为 HookEventEnvelope
/// 2. 调用 ccode-hooks 的 dispatcher
/// 3. 将 HookDecision 转换回 ccore 的 HookDecision
#[async_trait]
pub trait HookDispatcher: Send + Sync {
    /// 工具执行前 Hook
    ///
    /// 返回 Allow/Deny/Rewrite 决策。
    /// fail-open：Hook 执行失败时返回 Allow，不阻塞工具调用。
    async fn pre_tool_use(&self, ctx: &ToolCallContext) -> HookDecision;

    /// 工具执行后 Hook
    ///
    /// 记录工具执行结果，供后续审计或学习。
    /// fail-open：Hook 执行失败时仅记录 warn，不影响结果。
    async fn post_tool_use(&self, ctx: &ToolCallContext, result: &str, success: bool);
}

/// 空操作 HookDispatcher — 无 Hook 模式
///
/// 当 ccode-shell 未配置 Hook 时使用，所有调用直接放行。
pub struct NoOpHookDispatcher;

#[async_trait]
impl HookDispatcher for NoOpHookDispatcher {
    async fn pre_tool_use(&self, _ctx: &ToolCallContext) -> HookDecision {
        HookDecision::Allow
    }

    async fn post_tool_use(&self, _ctx: &ToolCallContext, _result: &str, _success: bool) {
        // 无操作
    }
}
