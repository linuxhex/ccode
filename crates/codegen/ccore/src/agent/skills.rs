//! Skill 系统 - 可复用的 prompt 模板 + 工具配置
//!
//! Skill 存储在 ~/.ccode/skills/ 目录下，每个 .md 文件代表一个 Skill。
//! 文件格式为 front-matter (YAML) + prompt 模板：
//!
//! ```markdown
//! ---
//! name: refactor
//! description: 代码重构助手
//! tools: [bash, read, write, edit]
//! model: ccode-3
//! ---
//! 你是一个代码重构专家...
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Skill 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Skill 名称
    pub name: String,
    /// 描述
    pub description: String,
    /// prompt 模板（支持 {{variable}} 占位符）
    pub prompt_template: String,
    /// 可用工具列表（空则使用默认工具集）
    pub tools: Vec<String>,
    /// 推荐模型
    pub recommended_model: Option<String>,
}

/// Skill 注册表
pub struct SkillRegistry {
    skills: HashMap<String, Skill>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            skills: HashMap::new(),
        };
        // 注册内置 Skill
        registry.register_builtin_skills();
        registry
    }

    /// 注册 skill
    pub fn register(&mut self, skill: Skill) {
        self.skills.insert(skill.name.clone(), skill);
    }

    /// 查找 skill
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// 列出所有 skill
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// 从目录加载 skill（.ccode/skills/ 目录下的 .md 文件）
    pub fn load_from_dir(&mut self, dir: &Path) -> anyhow::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                match Self::parse_skill_file(&path) {
                    Ok(skill) => {
                        tracing::info!("加载 Skill：{}", skill.name);
                        self.register(skill);
                    }
                    Err(e) => {
                        tracing::warn!("解析 Skill 文件 {:?} 失败：{}", path, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// 解析单个 Skill 文件（front-matter + markdown）
    fn parse_skill_file(path: &Path) -> anyhow::Result<Skill> {
        let content = std::fs::read_to_string(path)?;

        // 分离 front-matter 和 prompt 模板
        let (front_matter, prompt_template) = if let Some(rest) = content.strip_prefix("---") {
            if let Some(end_idx) = rest.find("---") {
                let fm = &rest[..end_idx];
                let template = rest[end_idx + 3..].trim().to_string();
                (fm.to_string(), template)
            } else {
                // 没有 front-matter 结束标记，整个文件作为 prompt
                (String::new(), content.trim().to_string())
            }
        } else {
            // 没有 front-matter，用文件名作为 name
            (String::new(), content.trim().to_string())
        };

        // 解析 front-matter
        let name = Self::extract_yaml_field(&front_matter, "name")
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
        let description = Self::extract_yaml_field(&front_matter, "description")
            .unwrap_or_else(|| "自定义 Skill".into());
        let tools = Self::extract_yaml_list(&front_matter, "tools");
        let recommended_model = Self::extract_yaml_field(&front_matter, "model");

        Ok(Skill {
            name,
            description,
            prompt_template,
            tools,
            recommended_model,
        })
    }

    /// 从 YAML front-matter 中提取字符串字段
    fn extract_yaml_field(front_matter: &str, field: &str) -> Option<String> {
        for line in front_matter.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(&format!("{}:", field)) {
                let value = rest.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        None
    }

    /// 从 YAML front-matter 中提取列表字段
    fn extract_yaml_list(front_matter: &str, field: &str) -> Vec<String> {
        let mut items = Vec::new();
        let mut in_list = false;

        for line in front_matter.lines() {
            let line = line.trim();
            if line.starts_with(&format!("{}:", field)) {
                // 内联列表格式：tools: [bash, read]
                if let Some(start) = line.find('[') {
                    if let Some(end) = line.find(']') {
                        let list_content = &line[start + 1..end];
                        for item in list_content.split(',') {
                            let item = item.trim().trim_matches('"').trim_matches('\'');
                            if !item.is_empty() {
                                items.push(item.to_string());
                            }
                        }
                    }
                }
                in_list = true;
                continue;
            }
            if in_list {
                // 多行列表格式：- bash
                if let Some(item) = line.strip_prefix("- ") {
                    let item = item.trim().trim_matches('"').trim_matches('\'');
                    if !item.is_empty() {
                        items.push(item.to_string());
                    }
                } else if !line.is_empty() {
                    in_list = false;
                }
            }
        }

        items
    }

    /// 注册内置 Skill
    fn register_builtin_skills(&mut self) {
        self.register(Skill {
            name: "debug".into(),
            description: "调试代码问题：定位 → 分析 → 修复".into(),
            prompt_template: "你是一个代码调试专家。\
                用户的代码出现了问题，请按照以下步骤调试：\n\
                1. 读取相关代码，理解上下文\n\
                2. 搜索错误信息，定位问题根因\n\
                3. 提出修复方案并实施\n\
                4. 验证修复是否有效\n\
                优先使用只读工具（read, grep）分析问题，确认后再使用编辑工具修复。".into(),
            tools: vec!["read".into(), "grep".into(), "bash".into(), "edit".into()],
            recommended_model: None,
        });

        self.register(Skill {
            name: "refactor".into(),
            description: "代码重构：改善代码结构而不改变行为".into(),
            prompt_template: "你是一个代码重构专家。\
                请按照以下原则重构代码：\n\
                1. 先读取并理解现有代码\n\
                2. 确保有测试覆盖（或先写测试）\n\
                3. 小步重构，每次只改一处\n\
                4. 每步重构后运行测试验证\n\
                不要改变外部行为，只改善内部结构。".into(),
            tools: vec!["read".into(), "edit".into(), "bash".into()],
            recommended_model: None,
        });

        self.register(Skill {
            name: "test-gen".into(),
            description: "自动生成单元测试".into(),
            prompt_template: "你是一个测试工程师。\
                请为指定代码生成单元测试：\n\
                1. 读取目标代码，理解所有公共接口\n\
                2. 为每个公共方法设计测试用例（正常/边界/异常）\n\
                3. 生成测试代码\n\
                4. 运行测试确保通过\n\
                测试应覆盖主要路径、边界条件和错误处理。".into(),
            tools: vec!["read".into(), "write".into(), "bash".into()],
            recommended_model: None,
        });

        self.register(Skill {
            name: "review".into(),
            description: "代码审查：检查质量、安全和性能问题".into(),
            prompt_template: "你是一个高级代码审查员。\
                请审查指定代码，关注以下方面：\n\
                1. 正确性：逻辑是否正确，边界条件是否处理\n\
                2. 安全性：是否有注入、泄露等安全风险\n\
                3. 性能：是否有明显的性能瓶颈\n\
                4. 可维护性：命名、结构、复杂度是否合理\n\
                5. 最佳实践：是否遵循语言和框架的惯用法\n\
                给出具体的问题和改进建议，按严重程度排序。".into(),
            tools: vec!["read".into(), "grep".into()],
            recommended_model: None,
        });
    }
}
