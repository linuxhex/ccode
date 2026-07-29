//! 降级模块入口（已迁移至仿生架构）
//!
//! [DEPRECATED] 降级策略已融合到 Kernel::ReflexRouter 的反射规则中。
//! 新代码请使用 kernel::reflex::ReflexRouter 替代。
//! 本模块保留以兼容现有调用方。

pub mod fallback;

pub use fallback::{DegradationConfig, DegradationStrategy};