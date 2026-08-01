//! 项目上下文工具 — 读取 CCODE.md 获取项目规范和约束
//!
//! 类似 Claude Code 的 CLAUDE.md，在项目根目录放置 CCODE.md 文件，
//! 包含项目规范、编码约束、架构决策等，在每次会话开始时自动注入。

use std::path::Path;

/// 项目上下文（从 CCODE.md 加载）
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    /// 原始内容
    pub content: String,
    /// 是否已加载
    pub loaded: bool,
}

impl ProjectContext {
    /// 从项目根目录加载 CCODE.md
    pub fn load_from_dir(dir: &Path) -> Self {
        let ccode_path = dir.join("CCODE.md");
        let claude_path = dir.join("CLAUDE.md");

        // 优先读取 CCODE.md，回退到 CLAUDE.md（兼容 Claude Code 项目）
        let path = if ccode_path.exists() {
            Some(ccode_path)
        } else if claude_path.exists() {
            Some(claude_path)
        } else {
            None
        };

        if let Some(path) = path {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    tracing::info!(
                        path = %path.display(),
                        len = content.len(),
                        "已加载项目上下文"
                    );
                    Self { content, loaded: true }
                }
                Err(e) => {
                    tracing::warn!("读取项目上下文失败：{}", e);
                    Self::default()
                }
            }
        } else {
            Self::default()
        }
    }

    /// 获取注入系统提示的内容
    pub fn system_prompt(&self) -> Option<String> {
        if self.loaded && !self.content.is_empty() {
            Some(format!("[项目规范]\n{}", self.content))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_load_cocode_md() {
        let dir = std::env::temp_dir();
        let path = dir.join("CCODE.md");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"# Test\nHello world").unwrap();

        let ctx = ProjectContext::load_from_dir(&dir);
        assert!(ctx.loaded);
        assert!(ctx.content.contains("Hello world"));
        assert!(ctx.system_prompt().is_some());

        std::fs::remove_file(&path).ok();
    }
}
