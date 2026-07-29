//! 性能优化模块入口（已迁移至仿生架构）
//!
//! [DEPRECATED] 性能优化能力已融合到对应器官：
//! - ConcurrencyController + MessagePool → Kernel::AutonomicNervousSystem
//! - BatchExecutor → HandNode（批量操作协调）
//! 新代码请使用器官 Node 的 API 替代。
//! 本模块保留以兼容现有调用方。

pub mod memory_pool;
pub mod concurrency;
pub mod batch_executor;

pub use memory_pool::MessagePool;
pub use concurrency::{ConcurrencyController, ConcurrencyConfig};
pub use batch_executor::BatchExecutor;