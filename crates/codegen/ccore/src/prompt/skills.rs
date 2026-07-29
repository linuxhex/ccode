//! 技能发现和注入系统（借鉴 Claude Code 设计）
//!
//! 从 `.skill/` 目录发现技能文件并格式化为系统提示注入文本。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// 技能文件信息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillInfo {
    /// 技能文件路径
    pub path: PathBuf,
    /// 技能名称（从文件名或 frontmatter 解析）
    pub name: String,
    /// 技能描述（从 frontmatter 或文件第一段解析）
    pub description: Option<String>,
    /// 技能内容
    pub content: String,
}

/// 从 .skill 目录发现技能文件
///
/// 搜索规则（借鉴 Claude Code）：
/// 1. 扫描工作目录下的 `.skill/` 目录
/// 2. 支持 `.skill/*.md` 文件（每个文件是一个技能）
/// 3. 解析 frontmatter 获取 name/description
/// 4. 如果没有 frontmatter，用文件名作为技能名
pub fn discover_skills(work_dir: &Path) -> Vec<SkillInfo> {
    let skill_dir = work_dir.join(".skill");

    if !skill_dir.is_dir() {
        return Vec::new();
    }

    let mut skills = Vec::new();

    // 扫描 .skill/ 目录下的所有 .md 文件
    if let Ok(entries) = std::fs::read_dir(&skill_dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // 只处理 .md 文件
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    // 解析 frontmatter
                    let (frontmatter, body) = parse_frontmatter(&content);

                    // 提取名称
                    let name = frontmatter
                        .as_ref()
                        .and_then(|fm| fm.get("name").cloned())
                        .unwrap_or_else(|| {
                            // 从文件名推导技能名（去除 .md 扩展名）
                            path.file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("unknown")
                                .to_string()
                        });

                    // 提取描述
                    let description = frontmatter
                        .as_ref()
                        .and_then(|fm| fm.get("description").cloned())
                        .or_else(|| extract_first_paragraph(&body));

                    skills.push(SkillInfo {
                        path: path.clone(),
                        name,
                        description,
                        content: body.trim().to_string(),
                    });
                }
            }
        }
    }

    // 按名称排序以保证稳定输出
    skills.sort_by(|a, b| a.name.cmp(&b.name));

    skills
}

/// 解析 YAML frontmatter
///
/// 返回 (frontmatter, body)
/// frontmatter 是一个 HashMap，包含解析的键值对
fn parse_frontmatter(content: &str) -> (Option<std::collections::HashMap<String, String>>, String) {
    // 检查是否以 --- 开头
    if !content.starts_with("---\n") {
        return (None, content.to_string());
    }

    // 查找结束的 ---
    let end_marker = content.find("\n---\n");
    if let Some(end_idx) = end_marker {
        let frontmatter_str = &content[4..end_idx];
        let body = &content[end_idx + 5..];

        // 解析 YAML Mapping
        let mapping: Option<serde_yaml::Mapping> =
            serde_yaml::from_str(frontmatter_str).ok();

        // 将 Mapping 转换为 HashMap<String, String>
        let frontmatter = mapping.and_then(|m| {
            let mut map = std::collections::HashMap::new();
            for (key, value) in m {
                if let (Some(key_str), Some(value_str)) = (key.as_str(), value.as_str()) {
                    map.insert(key_str.to_string(), value_str.to_string());
                }
            }
            if map.is_empty() {
                None
            } else {
                Some(map)
            }
        });

        (frontmatter, body.to_string())
    } else {
        (None, content.to_string())
    }
}

/// 从 Markdown 内容中提取第一段作为描述
fn extract_first_paragraph(content: &str) -> Option<String> {
    let reader = BufReader::new(content.as_bytes());
    let mut paragraph_lines = Vec::new();
    let mut in_paragraph = false;

    for line_result in reader.lines() {
        if let Ok(line) = line_result {
            // 跳过空行
            if line.trim().is_empty() {
                if in_paragraph {
                    // 第一段结束
                    break;
                }
                continue;
            }

            // 跳过标题（以 # 开头的行）
            if line.trim().starts_with('#') {
                if in_paragraph {
                    // 第一段结束
                    break;
                }
                continue;
            }

            // 收集段落内容
            in_paragraph = true;
            paragraph_lines.push(line.trim().to_string());
        }
    }

    if paragraph_lines.is_empty() {
        None
    } else {
        // 合并段落行
        Some(paragraph_lines.join(" "))
    }
}

/// 将发现的技能格式化为系统提示注入文本
///
/// 输出格式：
/// ```text
/// <agent_skills>
/// ## Available Skills
/// - **skill_name**: 技能描述（如果有的话）
///
/// ### skill_name.md
/// [技能文件内容]
/// </agent_skills>
/// ```
pub fn format_skills_section(skills: &[SkillInfo]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut section = String::from("<agent_skills>\n## Available Skills\n");

    // 1. 技能列表
    for skill in skills {
        if let Some(ref desc) = skill.description {
            section.push_str(&format!("- **{}**: {}\n", skill.name, desc));
        } else {
            section.push_str(&format!("- **{}**\n", skill.name));
        }
    }

    section.push('\n');

    // 2. 每个技能的详细内容
    for skill in skills {
        section.push_str(&format!("### {}.md\n", skill.name));
        section.push_str(&skill.content);
        section.push_str("\n\n");
    }

    section.push_str("</agent_skills>");

    Some(section)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\nname: my-skill\ndescription: A test skill\n---\n\nSkill body here.\n";
        let (fm, body) = parse_frontmatter(content);

        assert!(fm.is_some());
        let fm = fm.unwrap();
        assert_eq!(fm.get("name").map(|s| s.as_str()), Some("my-skill"));
        assert_eq!(
            fm.get("description").map(|s| s.as_str()),
            Some("A test skill")
        );
        assert_eq!(body.trim(), "Skill body here.");
    }

    #[test]
    fn test_parse_frontmatter_missing() {
        let content = "Just body content, no frontmatter.";
        let (fm, body) = parse_frontmatter(content);

        assert!(fm.is_none());
        assert_eq!(body, content);
    }

    #[test]
    fn test_extract_first_paragraph() {
        let content = "# Title\n\nFirst paragraph line one.\nFirst paragraph line two.\n\nSecond paragraph.";
        let desc = extract_first_paragraph(content);

        assert!(desc.is_some());
        assert_eq!(
            desc.unwrap(),
            "First paragraph line one. First paragraph line two."
        );
    }

    #[test]
    fn test_extract_first_paragraph_empty() {
        let content = "# Title\n\n## Section";
        let desc = extract_first_paragraph(content);

        assert!(desc.is_none());
    }

    #[test]
    fn test_discover_skills_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let skills = discover_skills(tmp.path());

        assert!(skills.is_empty());
    }

    #[test]
    fn test_discover_skills_with_files() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(".skill");
        fs::create_dir_all(&skill_dir).unwrap();

        // 创建技能文件
        let skill_content = "---\nname: test-skill\ndescription: A test skill\n---\n\nTest skill body.\n";
        fs::write(skill_dir.join("test-skill.md"), skill_content).unwrap();

        // 创建无 frontmatter 的技能
        fs::write(
            skill_dir.join("plain-skill.md"),
            "Plain skill without frontmatter.",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());

        assert_eq!(skills.len(), 2);

        // 检查第一个技能（按名称排序）
        assert_eq!(skills[0].name, "plain-skill");
        assert_eq!(
            skills[0].description,
            Some("Plain skill without frontmatter.".to_string())
        );

        assert_eq!(skills[1].name, "test-skill");
        assert_eq!(skills[1].description, Some("A test skill".to_string()));
    }

    #[test]
    fn test_format_skills_section_empty() {
        let skills: Vec<SkillInfo> = vec![];
        let result = format_skills_section(&skills);

        assert!(result.is_none());
    }

    #[test]
    fn test_format_skills_section() {
        let skills = vec![
            SkillInfo {
                path: PathBuf::from("/test/.skill/skill1.md"),
                name: "skill1".to_string(),
                description: Some("First skill".to_string()),
                content: "Content 1".to_string(),
            },
            SkillInfo {
                path: PathBuf::from("/test/.skill/skill2.md"),
                name: "skill2".to_string(),
                description: None,
                content: "Content 2".to_string(),
            },
        ];

        let result = format_skills_section(&skills).unwrap();

        assert!(result.contains("<agent_skills>"));
        assert!(result.contains("## Available Skills"));
        assert!(result.contains("- **skill1**: First skill"));
        assert!(result.contains("- **skill2**"));
        assert!(result.contains("### skill1.md"));
        assert!(result.contains("Content 1"));
        assert!(result.contains("### skill2.md"));
        assert!(result.contains("Content 2"));
        assert!(result.contains("</agent_skills>"));
    }

    #[test]
    fn test_discover_skills_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join(".skill");
        fs::create_dir_all(&skill_dir).unwrap();

        // 创建 .md 文件
        fs::write(skill_dir.join("skill.md"), "Skill content").unwrap();
        // 创建非 .md 文件
        fs::write(skill_dir.join("readme.txt"), "Not a skill").unwrap();

        let skills = discover_skills(tmp.path());

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "skill");
    }
}