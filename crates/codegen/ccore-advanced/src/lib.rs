//! ccore-advanced — 进阶模块
//!
//! 本 crate 包含 ccode 的进阶功能模块，编译为 cdylib + rlib。
//! - master 分支：完整源码编译
//! - main 分支：使用预编译的 .rlib/.so 二进制

pub mod agent;

pub mod metrics;
pub mod telemetry;
pub mod retry;
pub mod degradation;
pub mod performance;
pub mod error;
pub mod mcp_server;
pub mod utils;