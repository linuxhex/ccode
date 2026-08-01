//! ccode 核心库 - 分布式消息总线驱动的 AI 编程 Agent
//!
//! ## 架构概览
//!
//! 基于 ROS 1 风格双面架构（控制面 + 数据面分离）：
//! - **控制面**：Kernel ROUTER/PUB 处理 Node 注册、发现、心跳
//! - **数据面**：Node 间 PUB/SUB 直连，业务数据不经 Kernel 转发
//!
//! ## 核心模块
//!
//! | 模块 | 职责 |
//! |------|------|
//! | `kernel` | Broker 消息路由 + ANS 健康检查 + 背压流控 |
//! | `node` | 6 种 Node：Thinker/Sampler/Tool/State/TUI/Acp |
//! | `message` | MessagePack 帧编解码 + Topic 模式匹配 |
//! | `memory` | L0/L1/L2 三层记忆 + Context Engine（向量检索+依赖图+意图检索） |
//! | `agent` | LoopStateMachine + 4 层循环工程（Turn/Goal/Schedule/Proactive） |
//! | `sampler` | LLM Provider 适配 + 流式响应 + 模型切换 |
//! | `tools` | 权限链（5 阶段）+ Shell 安全（14 项检查）+ 工具执行 |
//!
//! ## 编译输出
//!
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
pub mod telemetry;
pub mod tracing;
pub mod retry;
pub mod degradation;
pub mod performance;
pub mod error;
pub mod mcp_server;
pub mod utils;
