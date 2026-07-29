//! ccode 核心库 - 消息总线驱动的终端 AI 编程代理
//!
//! 架构：微内核 + ZeroMQ PUB/SUB + REQ/REP 消息总线
//! - Kernel：消息路由器 + Node 生命周期管理
//! - Node：独立的功能模块（Agent/Sampler/Tool/State/TUI）
//! - 所有 Node 通过 ZMQ 消息总线通信
//!
//! 编译为 cdylib（动态库）+ rlib（静态库）双输出：
//! - cdylib：供 ccode-cli 通过 FFI 调用
//! - rlib：供其他 Rust crate 直接依赖

pub mod kernel;
pub mod node;
pub mod message;
pub mod memory;
pub mod sampler;
pub mod agent;
pub mod tools;
pub mod config;
pub mod ffi;
pub mod prompt;

// A+ 增强模块
pub mod persistence;
pub mod metrics;
pub mod tracing;
pub mod retry;
pub mod degradation;
pub mod performance;
