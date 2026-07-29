//! Tracing 模块入口
//!
//! 提供自定义 tracing 格式化器。

pub mod json_formatter;

pub use json_formatter::{JsonFormatter, SimpleJsonFormatter, init_json_logging};