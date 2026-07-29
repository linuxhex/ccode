//! Persona 注入系统
//!
//! 提供 persona 注册、管理和格式化功能，用于系统提示注入

use std::collections::HashMap;

/// Persona 信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersonaInfo {
    /// Persona 名称
    pub name: String,
    /// Persona 描述/角色定义
    pub description: String,
    /// Persona 指令（可选）
    pub instructions: Option<String>,
}

/// Persona 注册表（预定义的 personas）
pub struct PersonaRegistry {
    personas: HashMap<String, PersonaInfo>,
}

impl PersonaRegistry {
    /// 创建新的 PersonaRegistry 并注册默认 personas
    pub fn new() -> Self {
        let mut registry = Self {
            personas: HashMap::new(),
        };
        registry.register_defaults();
        registry
    }

    /// 注册预定义的 personas
    fn register_defaults(&mut self) {
        self.register(PersonaInfo {
            name: "reviewer".into(),
            description: "审查代码变更，提供结构化的审查意见".into(),
            instructions: Some(
                "审查时关注：正确性、性能、可维护性、安全性。输出格式：优点、问题、建议。".into()
            ),
        });
        self.register(PersonaInfo {
            name: "architect".into(),
            description: "设计系统架构，提供技术决策建议".into(),
            instructions: Some(
                "设计时考虑：可扩展性、性能、成本、安全。输出架构图和决策理由。".into()
            ),
        });
    }

    /// 注册一个 persona
    pub fn register(&mut self, persona: PersonaInfo) {
        self.personas.insert(persona.name.clone(), persona);
    }

    /// 根据名称获取 persona
    pub fn get(&self, name: &str) -> Option<&PersonaInfo> {
        self.personas.get(name)
    }

    /// 列出所有注册的 personas
    pub fn list(&self) -> Vec<&PersonaInfo> {
        self.personas.values().collect()
    }
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 将活跃的 personas 格式化为系统提示注入文本
///
/// 输出格式：
/// ```text
/// <personas>
/// ## Active Personas
///
/// ### reviewer
/// 审查代码变更，提供结构化的审查意见
///
/// 指令：审查时关注：正确性、性能、可维护性、安全性。输出格式：优点、问题、建议。
/// </personas>
/// ```
pub fn format_personas_section(personas: &[PersonaInfo]) -> Option<String> {
    if personas.is_empty() {
        return None;
    }

    let mut section = String::from("<personas>\n## Active Personas\n");

    for persona in personas {
        section.push_str(&format!("\n### {}\n", persona.name));
        section.push_str(&persona.description);
        section.push('\n');

        if let Some(ref instructions) = persona.instructions {
            section.push_str(&format!("\n指令：{}", instructions));
            section.push('\n');
        }
    }

    section.push_str("</personas>");
    Some(section)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persona_registry_defaults() {
        let registry = PersonaRegistry::new();
        assert!(registry.get("reviewer").is_some());
        assert!(registry.get("architect").is_some());
        assert_eq!(registry.list().len(), 2);
    }

    #[test]
    fn test_format_personas_section_empty() {
        let personas: Vec<PersonaInfo> = vec![];
        assert!(format_personas_section(&personas).is_none());
    }

    #[test]
    fn test_format_personas_section() {
        let personas = vec![
            PersonaInfo {
                name: "reviewer".into(),
                description: "审查代码变更".into(),
                instructions: Some("关注正确性".into()),
            },
        ];
        let output = format_personas_section(&personas).unwrap();
        assert!(output.contains("<personas>"));
        assert!(output.contains("### reviewer"));
        assert!(output.contains("审查代码变更"));
        assert!(output.contains("指令：关注正确性"));
        assert!(output.contains("</personas>"));
    }
}