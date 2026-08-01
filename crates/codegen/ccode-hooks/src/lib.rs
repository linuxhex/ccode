//! # ccode-hooks
//!
//! 权限决策链，5 阶段过滤与执行。
//!
//! | 阶段 | 说明 |
//! |---|---|
//! | pre-filter | 工具调用前预过滤 |
//! | hook | 运行时 Hook 发现与执行 |
//! | rule | 权限规则匹配与判定 |
//! | handler | 命名处理器分发 |
//! | deny-recovery | 拒绝后恢复策略 |

pub mod config;
pub mod discovery;
pub mod dispatcher;
mod env_expand;
pub mod error;
pub mod event;
pub mod matcher;
pub mod result;
pub mod runner;
#[cfg(test)]
mod test_support;
pub mod permission_rules;
pub mod permission_chain;
pub mod hook_rewrite;
pub mod trust;
