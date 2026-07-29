//! System prompt 模块
//!
//! 提供系统提示的构建和渲染功能

pub mod agents_md;
pub mod context;
pub mod personas;
pub mod skills;

pub use agents_md::{AgentConfigFile, discover_agents_md, format_agents_md_section};
pub use context::{PromptContext, TemplateMode};
pub use personas::{PersonaInfo, PersonaRegistry, format_personas_section};
pub use skills::{SkillInfo, discover_skills, format_skills_section};