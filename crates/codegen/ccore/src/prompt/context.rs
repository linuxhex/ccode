//! System prompt context for ccore agent.
//!
//! Borrowed from Claude Code's PromptContext design, adapted for ccore's
//! message-bus architecture without ToolBridge dependency.

use serde::{Deserialize, Serialize};
use crate::memory::working::WorkingMemory;

/// Controls which base template to use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TemplateMode {
    /// Standard agent template (full capabilities)
    #[default]
    Full,
    /// Compact template (after compaction, minimal context)
    Compact,
    /// Subagent template (limited capabilities, task-focused)
    Subagent,
}

/// Agent-specific inputs for system prompt rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptContext {
    /// Which template mode to use
    pub template_mode: TemplateMode,
    /// Working directory path
    pub working_directory: Option<String>,
    /// Current date (YYYY-MM-DD)
    pub current_date: Option<String>,
    /// OS name
    pub os_name: Option<String>,
    /// User's default shell
    pub shell_path: Option<String>,
    /// Custom instructions to append after base template
    pub custom_instructions: Option<String>,
    /// Tool definitions (rendered from ToolBridge)
    pub tools_section: Option<String>,
    /// Memory system enabled flag
    pub memory_enabled: bool,
}

impl Default for PromptContext {
    fn default() -> Self {
        Self {
            template_mode: TemplateMode::Full,
            working_directory: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .ok(),
            current_date: Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
            os_name: Some(std::env::consts::OS.to_string()),
            shell_path: std::env::var("SHELL").ok(),
            custom_instructions: None,
            tools_section: None,
            memory_enabled: false,
        }
    }
}

impl PromptContext {
    /// Create new PromptContext with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Render the full system prompt
    pub fn render(&self, working_memory: &WorkingMemory) -> String {
        let mut prompt = String::new();
        
        // 1. Base template header
        prompt.push_str(&self.render_template_header());
        
        // 2. User info section
        prompt.push_str(&self.render_user_info());
        
        // 3. Tools section
        if let Some(ref tools) = self.tools_section {
            prompt.push_str("\n\n## Available Tools\n\n");
            prompt.push_str(tools);
        }
        
        // 4. Custom instructions
        if let Some(ref instructions) = self.custom_instructions {
            prompt.push_str("\n\n## Custom Instructions\n\n");
            prompt.push_str(instructions);
        }
        
        // 5. Working memory context (if any hot entries)
        let hot_entries = working_memory.hot_entries_summary();
        if !hot_entries.is_empty() {
            prompt.push_str("\n\n## Recent Context\n\n");
            prompt.push_str(&hot_entries);
        }
        
        prompt
    }

    fn render_template_header(&self) -> String {
        match self.template_mode {
            TemplateMode::Full => include_str!("templates/full.md").to_string(),
            TemplateMode::Compact => include_str!("templates/compact.md").to_string(),
            TemplateMode::Subagent => include_str!("templates/subagent.md").to_string(),
        }
    }

    fn render_user_info(&self) -> String {
        let mut section = String::from("\n\n<user_info>\n");
        
        if let Some(ref wd) = self.working_directory {
            section.push_str(&format!("Working directory: {}\n", wd));
        }
        if let Some(ref date) = self.current_date {
            section.push_str(&format!("Current date: {}\n", date));
        }
        if let Some(ref os) = self.os_name {
            section.push_str(&format!("OS: {}\n", os));
        }
        if let Some(ref shell) = self.shell_path {
            section.push_str(&format!("Shell: {}\n", shell));
        }
        
        section.push_str("</user_info>");
        section
    }

    /// Set custom instructions
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.custom_instructions = Some(instructions.into());
        self
    }

    /// Set tools section
    pub fn with_tools(mut self, tools: impl Into<String>) -> Self {
        self.tools_section = Some(tools.into());
        self
    }
}