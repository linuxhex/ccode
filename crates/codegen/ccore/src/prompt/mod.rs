//! System prompt 模块
//!
//! 提供系统提示的构建和渲染功能
//!
//! AGENTS.md / CLAUDE.md / CCODE.md 的发现与注入不在这里 —— 见
//! [`crate::tools::project_context`]，那是主线（`ThinkerNode::load_project_context`）
//! 实际调用的实现。此处曾有一份零调用者的 `agents_md` 副本，已删除。

pub mod context;
pub mod personas;
pub mod skills;

pub use context::{PromptContext, TemplateMode};
pub use personas::{PersonaInfo, PersonaRegistry, format_personas_section};
pub use skills::{SkillInfo, discover_skills, format_skills_section};