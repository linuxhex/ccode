//! System prompt context for ccore agent.
//!
//! Borrowed from Claude Code's PromptContext design, adapted for ccore's
//! message-bus architecture without ToolBridge dependency.

use notify::Watcher;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use crate::memory::working::WorkingMemory;
use crate::prompt::agents_md::AgentConfigFile;
use crate::prompt::skills::SkillInfo;
use crate::prompt::personas::PersonaInfo;
use crate::tools::prompts;

/// 模板渲染器（借鉴 Claude Code TemplateRenderer）
///
/// 支持动态变量注入：
/// - ${{ tools.by_kind.read }} → 实际工具名
/// - ${{ tools.by_kind.edit }} → 实际工具名
/// - ${{ os_name }} → 操作系统名
/// - ${{ memory_enabled }} → 是否启用记忆
pub struct TemplateRenderer {
    /// 工具名映射 (kind → name)
    tool_names: HashMap<String, String>,
    /// 上下文变量
    variables: HashMap<String, String>,
}

impl TemplateRenderer {
    pub fn new() -> Self {
        let mut tool_names = HashMap::new();
        tool_names.insert("read".into(), "read".into());
        tool_names.insert("write".into(), "write".into());
        tool_names.insert("edit".into(), "edit".into());
        tool_names.insert("bash".into(), "bash".into());
        tool_names.insert("grep".into(), "grep".into());
        tool_names.insert("glob".into(), "glob".into());
        tool_names.insert("list_dir".into(), "list_dir".into());

        let mut variables = HashMap::new();
        variables.insert("os_name".into(), std::env::consts::OS.into());
        variables.insert("memory_enabled".into(), "true".into());

        Self { tool_names, variables }
    }

    /// 注册工具名
    pub fn register_tool_name(&mut self, kind: &str, name: &str) {
        self.tool_names.insert(kind.into(), name.into());
    }

    /// 设置变量
    pub fn set_variable(&mut self, key: &str, value: &str) {
        self.variables.insert(key.into(), value.into());
    }

    /// 渲染模板
    pub fn render(&self, template: &str) -> String {
        let mut result = template.to_string();

        // 替换 ${{ tools.by_kind.X }}
        for (kind, name) in &self.tool_names {
            let pattern = format!("${{{{ tools.by_kind.{} }}}}", kind);
            result = result.replace(&pattern, name);
        }

        // 替换 ${{ variable }}
        for (key, value) in &self.variables {
            let pattern = format!("${{{{ {} }}}}", key);
            result = result.replace(&pattern, value);
        }

        result
    }
}

impl Default for TemplateRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TemplateRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateRenderer")
            .field("tool_names_count", &self.tool_names.len())
            .field("variables_count", &self.variables.len())
            .finish()
    }
}

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

/// 模板文件监听器（热重载）
///
/// 借鉴 Claude Code 的模板热重载机制，但简化实现：
/// - 使用 notify crate 监听模板文件变化
/// - 文件变更时重新加载模板内容
/// - 无需加密（ccore 是开源项目）
pub struct TemplateWatcher {
    /// 文件系统监听器
    #[allow(dead_code)]  // 保留 watcher 以维持文件监听生命周期
    watcher: notify::RecommendedWatcher,
    /// 当前模板内容缓存
    templates: Arc<RwLock<HashMap<String, String>>>,
    /// 模板目录路径
    template_dir: PathBuf,
}

impl std::fmt::Debug for TemplateWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateWatcher")
            .field("template_dir", &self.template_dir)
            // SAFETY: RwLock is never poisoned in this module
            .field("templates_count", &self.templates.read().unwrap().len())
            .finish()
    }
}

impl TemplateWatcher {
    /// 创建新的模板监听器
    ///
    /// # 参数
    /// - `template_dir`: 模板目录路径
    ///
    /// # 返回
    /// 成功返回 TemplateWatcher，失败返回错误
    pub fn new(template_dir: PathBuf) -> Result<Self, anyhow::Error> {
        let templates = Arc::new(RwLock::new(HashMap::new()));

        // 初始加载所有模板
        let mut initial_templates = HashMap::new();
        if template_dir.exists() {
            for entry in std::fs::read_dir(&template_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().map(|e| e == "md").unwrap_or(false) {
                    let name = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !name.is_empty() {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            initial_templates.insert(name, content);
                        }
                    }
                }
            }
        }
        // SAFETY: RwLock is never poisoned in this module
        *templates.write().unwrap() = initial_templates;

        // 创建文件监听器
        let templates_clone = Arc::clone(&templates);
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                if event.kind.is_modify() || event.kind.is_create() {
                    // 重新加载变更的模板
                    for path in &event.paths {
                        if path.extension().map(|e| e == "md").unwrap_or(false) {
                            if let Ok(content) = std::fs::read_to_string(path) {
                                let name = path.file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                if !name.is_empty() {
                                    if let Ok(mut guard) = templates_clone.write() {
                                        guard.insert(name, content);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })?;

        if template_dir.exists() {
            watcher.watch(&template_dir, notify::RecursiveMode::NonRecursive)?;
        }

        Ok(Self {
            watcher,
            templates,
            template_dir,
        })
    }

    /// 获取模板内容
    ///
    /// # 参数
    /// - `name`: 模板文件名
    ///
    /// # 返回
    /// 模板内容的克隆
    pub fn get(&self, name: &str) -> Option<String> {
        // SAFETY: RwLock is never poisoned in this module
        self.templates.read().unwrap().get(name).cloned()
    }

    /// 获取模板目录路径
    pub fn template_dir(&self) -> &PathBuf {
        &self.template_dir
    }
}

/// 动态模板渲染器（超越 Claude Code）
///
/// 核心能力：
/// 1. 热重载：模板文件变更时自动更新
/// 2. 动态注入：根据运行时状态注入内容
/// 3. 条件渲染：根据 agent 类型选择不同模板片段
pub struct DynamicTemplateRenderer {
    /// 模板监听器
    watcher: TemplateWatcher,
}

impl std::fmt::Debug for DynamicTemplateRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicTemplateRenderer")
            .field("watcher", &self.watcher)
            .finish()
    }
}

impl DynamicTemplateRenderer {
    /// 创建新的动态模板渲染器
    ///
    /// # 参数
    /// - `template_dir`: 模板目录路径
    ///
    /// # 返回
    /// 成功返回渲染器，失败返回错误
    pub fn new(template_dir: PathBuf) -> Result<Self, anyhow::Error> {
        let watcher = TemplateWatcher::new(template_dir)?;
        Ok(Self { watcher })
    }

    /// 渲染系统提示（动态）
    ///
    /// 借鉴 Claude Code 的 render_with_extra，但更灵活：
    /// 1. 加载基础模板
    /// 2. 注入动态内容（工具定义、当前日期、工作目录）
    /// 3. 条件渲染（如果是 subagent，跳过某些部分）
    ///
    /// # 参数
    /// - `context`: 提示上下文
    /// - `working_memory`: 工作记忆
    ///
    /// # 返回
    /// 渲染后的提示字符串
    pub fn render(&self, context: &PromptContext, working_memory: &WorkingMemory) -> String {
        let template_name = match context.template_mode {
            TemplateMode::Full => "full.md",
            TemplateMode::Compact => "compact.md",
            TemplateMode::Subagent => "subagent.md",
        };

        // 尝试从监听器获取模板，失败则使用内嵌模板
        let mut template = self.watcher.get(template_name)
            .unwrap_or_else(|| match context.template_mode {
                TemplateMode::Full => include_str!("templates/full.md").to_string(),
                TemplateMode::Compact => include_str!("templates/compact.md").to_string(),
                TemplateMode::Subagent => include_str!("templates/subagent.md").to_string(),
            });

        // 动态注入占位符
        template = self.inject_dynamic_content(template, context);

        // 注入工作记忆
        template = self.inject_working_memory(template, working_memory);

        template
    }

    /// 注入动态内容（工具、日期、工作目录等）
    fn inject_dynamic_content(&self, mut template: String, context: &PromptContext) -> String {
        // 替换占位符：${{ tools_section }}
        if let Some(ref tools) = context.tools_section {
            template = template.replace("${{ tools_section }}", tools);
        } else {
            template = template.replace("${{ tools_section }}", "");
        }

        // 替换占位符：${{ current_date }}
        if let Some(ref date) = context.current_date {
            template = template.replace("${{ current_date }}", date);
        } else {
            template = template.replace("${{ current_date }}", "");
        }

        // 替换占位符：${{ working_directory }}
        if let Some(ref wd) = context.working_directory {
            template = template.replace("${{ working_directory }}", wd);
        } else {
            template = template.replace("${{ working_directory }}", "");
        }

        // 替换占位符：${{ custom_instructions }}
        if let Some(ref instructions) = context.custom_instructions {
            template = template.replace("${{ custom_instructions }}", instructions);
        } else {
            template = template.replace("${{ custom_instructions }}", "");
        }

        // 替换占位符：${{ os_name }}
        if let Some(ref os) = context.os_name {
            template = template.replace("${{ os_name }}", os);
        } else {
            template = template.replace("${{ os_name }}", "");
        }

        // 替换占位符：${{ shell_path }}
        if let Some(ref shell) = context.shell_path {
            template = template.replace("${{ shell_path }}", shell);
        } else {
            template = template.replace("${{ shell_path }}", "");
        }

        template
    }

    /// 注入工作记忆（热条目摘要）
    fn inject_working_memory(&self, mut template: String, working_memory: &WorkingMemory) -> String {
        // 注入热条目摘要
        let hot_summary = working_memory.hot_entries_summary();
        if !hot_summary.is_empty() {
            template = template.replace("${{ recent_context }}", &format!(
                "\n## Recent Context\n\n{}\n",
                hot_summary
            ));
        } else {
            template = template.replace("${{ recent_context }}", "");
        }

        template
    }
}

/// Agent-specific inputs for system prompt rendering.
#[derive(Debug, Serialize, Deserialize)]
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
    /// Discovered AGENTS.md files
    pub agents_md_files: Vec<AgentConfigFile>,
    /// Discovered skills from .skill/ directory
    pub skills: Vec<SkillInfo>,
    /// Active personas for system prompt injection
    pub active_personas: Vec<PersonaInfo>,
    /// Dynamic template renderer (optional, for hot reload)
    #[serde(skip)]
    pub renderer: Option<DynamicTemplateRenderer>,
    /// 模板渲染器（动态工具名注入）
    #[serde(skip)]
    pub template_renderer: TemplateRenderer,
}

impl Clone for PromptContext {
    fn clone(&self) -> Self {
        Self {
            template_mode: self.template_mode.clone(),
            working_directory: self.working_directory.clone(),
            current_date: self.current_date.clone(),
            os_name: self.os_name.clone(),
            shell_path: self.shell_path.clone(),
            custom_instructions: self.custom_instructions.clone(),
            tools_section: self.tools_section.clone(),
            memory_enabled: self.memory_enabled,
            agents_md_files: self.agents_md_files.clone(),
            skills: self.skills.clone(),
            active_personas: self.active_personas.clone(),
            renderer: None, // Renderer 不参与 Clone，设为 None
            template_renderer: TemplateRenderer::new(), // 重新创建默认渲染器
        }
    }
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
            agents_md_files: Vec::new(),
            skills: Vec::new(),
            active_personas: Vec::new(),
            renderer: None,
            template_renderer: TemplateRenderer::new(),
        }
    }
}

impl PromptContext {
    /// Create new PromptContext with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable hot reload for templates
    ///
    /// # 参数
    /// - `template_dir`: 模板目录路径
    ///
    /// # 返回
    /// 更新后的 PromptContext
    pub fn with_hot_reload(mut self, template_dir: PathBuf) -> Self {
        self.renderer = DynamicTemplateRenderer::new(template_dir).ok();
        self
    }

    /// Render the full system prompt
    pub fn render(&self, working_memory: &WorkingMemory) -> String {
        // 如果有动态渲染器，使用它
        if let Some(ref renderer) = self.renderer {
            return renderer.render(self, working_memory);
        }

        // 否则使用原有的静态渲染方式
        let mut prompt = String::new();

        // 1. Base template header
        prompt.push_str(&self.render_template_header());

        // 2. User info section
        prompt.push_str(&self.render_user_info());

        // 3. AGENTS.md section (after user_info)
        if let Some(agents_md) = crate::prompt::agents_md::format_agents_md_section(&self.agents_md_files) {
            prompt.push_str("\n\n");
            prompt.push_str(&agents_md);
        }

        // 4. Skills section (after agents_md)
        if let Some(skills_section) = crate::prompt::skills::format_skills_section(&self.skills) {
            prompt.push_str("\n\n");
            prompt.push_str(&skills_section);
        }

        // 5. Active personas section (after skills)
        if let Some(personas_section) = crate::prompt::personas::format_personas_section(&self.active_personas) {
            prompt.push_str("\n\n");
            prompt.push_str(&personas_section);
        }

        // 5. Tools section（如果未手动设置 tools_section，则从 prompts::get_tool_definitions() 自动生成）
        if let Some(ref tools) = self.tools_section {
            prompt.push_str("\n\n## Available Tools\n\n");
            prompt.push_str(tools);
        } else {
            let tool_defs = prompts::get_tool_definitions();
            if !tool_defs.is_empty() {
                prompt.push_str("\n\n## Available Tools\n\n");
                for def in &tool_defs {
                    let name = def["name"].as_str().unwrap_or("");
                    let description = def["description"].as_str().unwrap_or("");
                    prompt.push_str(&format!("- **{}**: {}\n", name, description));
                }
            }
        }

        // 6. Custom instructions
        if let Some(ref instructions) = self.custom_instructions {
            prompt.push_str("\n\n## Custom Instructions\n\n");
            prompt.push_str(instructions);
        }

        // 7. Working memory context (if any hot entries)
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

    /// Set working directory
    pub fn with_working_directory(mut self, dir: impl Into<String>) -> Self {
        self.working_directory = Some(dir.into());
        self
    }

    /// Add a persona to active personas
    pub fn with_persona(mut self, persona: PersonaInfo) -> Self {
        self.active_personas.push(persona);
        self
    }

    /// 使用 TemplateRenderer 渲染系统提示
    ///
    /// 借鉴 Claude Code 的 TemplateRenderer，支持动态工具名注入。
    /// 先用 TemplateRenderer 渲染占位符，再走正常的 render 流程。
    pub fn render_system_prompt(&self, working_memory: &WorkingMemory) -> String {
        let raw_prompt = self.render(working_memory);
        self.template_renderer.render(&raw_prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::io::Write;

    /// 测试 TemplateWatcher 基本功能
    #[test]
    fn test_template_watcher_basic() {
        // 创建临时目录
        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建测试模板文件
        let template_path = template_dir.join("test.md");
        let mut file = std::fs::File::create(&template_path).unwrap();
        file.write_all(b"Test template content").unwrap();

        // 创建监听器
        let watcher = TemplateWatcher::new(template_dir.clone()).unwrap();

        // 验证可以读取模板
        let content = watcher.get("test.md");
        assert!(content.is_some());
        assert_eq!(content.unwrap(), "Test template content");
    }

    /// 测试 TemplateWatcher 处理不存在的目录
    #[test]
    fn test_template_watcher_nonexistent_dir() {
        let watcher = TemplateWatcher::new(PathBuf::from("/nonexistent/path")).unwrap();
        // 应该不会崩溃，但返回 None
        assert!(watcher.get("test.md").is_none());
    }

    /// 测试 DynamicTemplateRenderer 注入内容
    #[test]
    fn test_dynamic_renderer_injection() {
        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建包含占位符的模板
        let template_path = template_dir.join("full.md");
        let mut file = std::fs::File::create(&template_path).unwrap();
        file.write_all(b"Template: ${{ tools_section }} - ${{ current_date }}").unwrap();

        let renderer = DynamicTemplateRenderer::new(template_dir).unwrap();

        let context = PromptContext {
            template_mode: TemplateMode::Full,
            tools_section: Some("my_tools".to_string()),
            current_date: Some("2025-01-15".to_string()),
            ..Default::default()
        };

        let working_memory = WorkingMemory::new(100);

        let result = renderer.render(&context, &working_memory);

        assert!(result.contains("my_tools"));
        assert!(result.contains("2025-01-15"));
        assert!(!result.contains("${{"));
    }

    /// 测试 PromptContext 使用动态渲染器
    #[test]
    fn test_prompt_context_with_hot_reload() {
        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建模板
        let template_path = template_dir.join("full.md");
        let mut file = std::fs::File::create(&template_path).unwrap();
        file.write_all(b"Dynamic template: ${{ working_directory }}").unwrap();

        let context = PromptContext::new()
            .with_hot_reload(template_dir)
            .with_working_directory("/test/path");

        let working_memory = WorkingMemory::new(100);
        let result = context.render(&working_memory);

        assert!(result.contains("/test/path"));
    }

    /// 测试占位符替换为空值
    #[test]
    fn test_injection_with_none_values() {
        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建模板
        let template_path = template_dir.join("full.md");
        let mut file = std::fs::File::create(&template_path).unwrap();
        file.write_all(b"Template: ${{ tools_section }}-${{ current_date }}").unwrap();

        let renderer = DynamicTemplateRenderer::new(template_dir).unwrap();

        let context = PromptContext {
            template_mode: TemplateMode::Full,
            tools_section: None,
            current_date: None,
            ..Default::default()
        };

        let working_memory = WorkingMemory::new(100);
        let result = renderer.render(&context, &working_memory);

        // 占位符应该被替换为空字符串
        assert!(!result.contains("${{"));
        assert!(!result.contains("tools_section"));
        assert!(!result.contains("current_date"));
    }

    /// 测试工作记忆注入
    #[test]
    fn test_working_memory_injection() {
        use crate::memory::working::MessageRole;

        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建模板
        let template_path = template_dir.join("full.md");
        let mut file = std::fs::File::create(&template_path).unwrap();
        file.write_all(b"Template: ${{ recent_context }}").unwrap();

        let renderer = DynamicTemplateRenderer::new(template_dir).unwrap();

        let context = PromptContext::default();
        let mut working_memory = WorkingMemory::new(100);

        // 添加热条目
        working_memory.push_hot(MessageRole::User, "test entry".to_string(), 10);

        let result = renderer.render(&context, &working_memory);

        // 应该包含 Recent Context 部分
        assert!(result.contains("Recent Context"));
        assert!(result.contains("test entry"));
    }

    /// 测试不同模板模式
    #[test]
    fn test_different_template_modes() {
        let temp_dir = TempDir::new().unwrap();
        let template_dir = temp_dir.path().to_path_buf();

        // 创建三个模板
        for (name, content) in &[
            ("full.md", "Full template"),
            ("compact.md", "Compact template"),
            ("subagent.md", "Subagent template"),
        ] {
            let template_path = template_dir.join(name);
            let mut file = std::fs::File::create(&template_path).unwrap();
            file.write_all(content.as_bytes()).unwrap();
        }

        let renderer = DynamicTemplateRenderer::new(template_dir).unwrap();

        // 测试 Full 模式
        let full_context = PromptContext {
            template_mode: TemplateMode::Full,
            ..Default::default()
        };
        let working_memory = WorkingMemory::new(100);
        let result = renderer.render(&full_context, &working_memory);
        assert!(result.contains("Full template"));

        // 测试 Compact 模式
        let compact_context = PromptContext {
            template_mode: TemplateMode::Compact,
            ..Default::default()
        };
        let result = renderer.render(&compact_context, &working_memory);
        assert!(result.contains("Compact template"));

        // 测试 Subagent 模式
        let subagent_context = PromptContext {
            template_mode: TemplateMode::Subagent,
            ..Default::default()
        };
        let result = renderer.render(&subagent_context, &working_memory);
        assert!(result.contains("Subagent template"));
    }
}