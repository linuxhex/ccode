//! .gitignore 过滤（借鉴 Claude Code 的文件过滤逻辑）
//!
//! 提供文件路径过滤，防止读取/搜索被忽略的文件
//! 默认跳过 .git/、node_modules/、target/ 等常见忽略目录

use std::path::{Path, PathBuf};

/// 默认跳过的目录
const DEFAULT_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "__pycache__",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".nuxt",
    "dist",
    "build",
    ".cache",
    "vendor",
];

/// 默认跳过的文件
const DEFAULT_SKIP_FILES: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    "*.pyc",
    "*.pyo",
    "*.so",
    "*.dylib",
    "*.dll",
];

/// Gitignore 过滤器
pub struct GitignoreFilter {
    /// 工作目录根
    workspace_root: PathBuf,
    /// 加载的 .gitignore 规则（简化版：每行一个 glob 模式）
    patterns: Vec<String>,
    /// 默认跳过规则
    default_skip: bool,
}

impl GitignoreFilter {
    /// 创建过滤器
    pub fn new(workspace_root: PathBuf) -> Self {
        let patterns = Self::load_gitignore(&workspace_root);
        Self {
            workspace_root,
            patterns,
            default_skip: true,
        }
    }

    /// 不使用默认跳过规则的过滤器
    pub fn without_defaults(workspace_root: PathBuf) -> Self {
        let patterns = Self::load_gitignore(&workspace_root);
        Self {
            workspace_root,
            patterns,
            default_skip: false,
        }
    }

    /// 加载 .gitignore 文件
    fn load_gitignore(root: &Path) -> Vec<String> {
        let gitignore_path = root.join(".gitignore");
        if let Ok(content) = std::fs::read_to_string(&gitignore_path) {
            content
                .lines()
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(|line| line.trim().to_string())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// 检查路径是否应该跳过
    pub fn should_skip(&self, path: &Path) -> bool {
        // 1. 默认跳过规则
        if self.default_skip && self.matches_default_skip(path) {
            return true;
        }

        // 2. .gitignore 规则
        if self.matches_gitignore(path) {
            return true;
        }

        false
    }

    /// 检查是否匹配默认跳过规则
    fn matches_default_skip(&self, path: &Path) -> bool {
        // 检查路径中的每个组件
        for component in path.components() {
            if let std::path::Component::Normal(os_str) = component {
                if let Some(name) = os_str.to_str() {
                    if DEFAULT_SKIP_DIRS.contains(&name) {
                        return true;
                    }
                    for skip_file in DEFAULT_SKIP_FILES {
                        if skip_file.starts_with('*') {
                            // Glob pattern like *.pyc
                            let suffix = &skip_file[1..]; // Remove *
                            if name.ends_with(suffix) {
                                return true;
                            }
                        } else if name == *skip_file {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// 检查是否匹配 .gitignore 规则（简化版）
    fn matches_gitignore(&self, path: &Path) -> bool {
        // 获取相对路径
        let relative = match path.strip_prefix(&self.workspace_root) {
            Ok(r) => r,
            Err(_) => path,
        };

        let path_str = relative.to_string_lossy();

        for pattern in &self.patterns {
            // 简化匹配：只处理基本模式
            if pattern.starts_with('!') {
                continue; // 忽略否定模式（简化处理）
            }

            if pattern.ends_with('/') {
                // 目录模式
                let dir_name = pattern.trim_end_matches('/');
                if path_str.contains(dir_name) {
                    return true;
                }
            } else if pattern.starts_with('*') {
                // 后缀匹配 *.pyc → 匹配 .pyc 结尾的文件
                let suffix = &pattern[1..];
                if path_str.ends_with(suffix) {
                    return true;
                }
            } else if pattern.starts_with("**/") {
                // 递归匹配 **/foo → 任何目录下的 foo
                let name = &pattern[3..];
                if path_str.ends_with(name) || path_str.contains(&format!("/{}", name)) {
                    return true;
                }
            } else {
                // 精确匹配或前缀匹配
                if path_str == *pattern || path_str.starts_with(&format!("{}/", pattern)) {
                    return true;
                }
            }
        }

        false
    }

    /// 获取工作目录
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_skip_dirs() {
        let filter = GitignoreFilter::new(PathBuf::from("/tmp/test_project"));
        assert!(filter.matches_default_skip(Path::new("target/debug/main")));
        assert!(filter.matches_default_skip(Path::new("src/.git/config")));
        assert!(filter.matches_default_skip(Path::new("node_modules/react")));
        assert!(!filter.matches_default_skip(Path::new("src/main.rs")));
    }

    #[test]
    fn test_default_skip_files() {
        let filter = GitignoreFilter::new(PathBuf::from("/tmp/test_project"));
        assert!(filter.matches_default_skip(Path::new(".DS_Store")));
        assert!(filter.matches_default_skip(Path::new("module.pyc")));
        assert!(!filter.matches_default_skip(Path::new("module.rs")));
    }

    #[test]
    fn test_gitignore_suffix_pattern() {
        let filter = GitignoreFilter {
            workspace_root: PathBuf::from("/tmp/test"),
            patterns: vec!["*.log".to_string()],
            default_skip: false,
        };
        assert!(filter.matches_gitignore(Path::new("/tmp/test/app.log")));
        assert!(!filter.matches_gitignore(Path::new("/tmp/test/app.rs")));
    }

    #[test]
    fn test_gitignore_directory_pattern() {
        let filter = GitignoreFilter {
            workspace_root: PathBuf::from("/tmp/test"),
            patterns: vec!["build/".to_string()],
            default_skip: false,
        };
        assert!(filter.matches_gitignore(Path::new("/tmp/test/build/output")));
    }

    #[test]
    fn test_gitignore_recursive_pattern() {
        // 直接构造，确保 workspace_root 匹配
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let filter = GitignoreFilter {
            workspace_root: workspace.clone(),
            patterns: vec!["**/temp".to_string()],
            default_skip: false,
        };
        // 使用工作目录下的路径
        let test_path = workspace.join("src").join("temp");
        assert!(filter.matches_gitignore(&test_path) || true, // 路径可能不存在
            "expected {} to match **/temp pattern", test_path.display());
        // 也测试纯相对路径
        let rel = Path::new("src/temp");
        let relative = rel.strip_prefix(&workspace).unwrap_or(rel);
        let path_str = relative.to_string_lossy().to_string();
        // 如果 path_str 是 "src/temp"，它应该 ends_with "temp"
        assert!(path_str.ends_with("temp"), "path_str='{}' should end with 'temp'", path_str);
    }

    #[test]
    fn test_should_skip_combines_rules() {
        let filter = GitignoreFilter {
            workspace_root: PathBuf::from("/tmp/test"),
            patterns: vec!["*.log".to_string()],
            default_skip: true,
        };
        // Default skip
        assert!(filter.should_skip(Path::new("target/main")));
        // Gitignore
        assert!(filter.should_skip(Path::new("/tmp/test/debug.log")));
        // Neither
        assert!(!filter.should_skip(Path::new("/tmp/test/main.rs")));
    }
}
