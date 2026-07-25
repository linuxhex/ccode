//! ccode 核心库 - 消息总线驱动的终端 AI 编程代理
//!
//! 架构：微内核 + ZeroMQ PUB/SUB + REQ/REP 消息总线
//! 所有功能模块作为独立 Node 进程，通过消息总线通信

pub mod message;
pub mod node;
pub mod kernel;
pub mod memory;
pub mod sampler;
pub mod agent;
pub mod tools;
pub mod config;
pub mod ffi;
