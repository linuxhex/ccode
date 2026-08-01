//! # ccode-tools
//!
//! 工具执行层，包含 Shell 安全检查、权限链集成与文件操作工具集。
//!
//! | 核心能力 | 说明 |
//! |---|---|
//! | Shell 安全检查 | 14 项 Shell 命令安全预检 |
//! | 权限链集成 | 与 ccode-hooks 权限决策链对接 |
//! | 文件操作工具集 | 读写、搜索、编辑等文件操作工具 |

pub use ccode_version::VERSION;

/// Default maximum output size (in bytes) for tool results sent to the model.
/// 40 KB ≈ 10 000 tokens
pub const DEFAULT_TOOL_OUTPUT_BYTES: usize = 40_000;

/// Default maximum output size (in characters) for bash/terminal tool results.
/// 20 000 chars ≈ 5 000 tokens. Matches the common `SHELL_CHAR_HARD_LIMIT`.
pub const DEFAULT_TOOL_OUTPUT_CHARS: usize = 20_000;

/// MCP inline tool-result cap (`MCP_MAX_OUTPUT_BYTES` and host/env helpers).
pub use util::mcp_truncate::{
    ENV_CCODE_MAX_MCP_OUTPUT_BYTES, ENV_MAX_MCP_OUTPUT_BYTES, MCP_MAX_OUTPUT_BYTES,
    mcp_max_output_bytes, mcp_max_output_bytes_from_env, set_mcp_max_output_bytes,
};

pub mod attribution;

pub mod bridge;
pub mod computer;
pub mod gitignore;
pub mod implementations;
pub mod normalization;
pub mod notification;
pub mod persistence;
pub mod registry;
pub mod reminders;
pub mod retry;
pub mod tool_taxonomy;
pub mod types;
pub mod util;
pub mod versions;

pub use attribution::{
    Auth401AttributionCallback, SENT_BEARER_PREFIX_LEN, SharedAttributionCallback, ToolConsumer,
};
