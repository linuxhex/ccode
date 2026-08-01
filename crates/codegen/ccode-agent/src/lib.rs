//! # ccode-agent
//!
//! Agent 策略层，涵盖 Prompt 构建、Doom Loop 检测、Skill 系统与经验学习。
//!
//! | 核心能力 | 说明 |
//! |---|---|
//! | Prompt 构建 | 系统提示词组装与压缩策略 |
//! | Doom Loop 检测 | 循环状态检测与自动打断 |
//! | Skill 系统 | 技能发现、加载与调度 |
//! | 经验学习 | 历史经验积累与复用 |

pub mod agent;
pub mod builder;
pub mod compaction;
pub mod config;
pub mod discovery;
pub mod error;
pub mod loop_state;
pub mod plugins;
pub mod prompt;
pub mod repo;
pub mod system_reminder;
pub mod timing;

pub use agent::Agent;
pub use builder::AgentBuilder;
pub use compaction::CompactionPolicy;
pub use config::AgentDefinition;
pub use config::preset_names;
pub use config::toolset_for_preset;
pub use config::workspace_ccode_build_toolset;
pub use error::AgentBuildError;
pub use prompt::context::{DEFAULT_SYSTEM_PROMPT_LABEL, PromptContext};
pub use system_reminder::ReminderPolicy;
