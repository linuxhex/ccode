//! Repo Map — 文件级依赖图（借鉴 Aider repo-map + Augment Context Engine 依赖追踪）
//!
//! 基于静态分析构建代码库的文件级依赖关系图：
//! - 解析 import/require/use 语句
//! - 构建文件→文件的依赖有向图
//! - 支持按文件查找上下游依赖链
//! - 支持影响范围分析（修改某文件会影响哪些文件）
//!
//! 与 Aider 的 repo-map 区别：
//! - Aider: 基于 tree-sitter AST 解析，输出 tags 格式给 LLM
//! - ccode: 正则解析 import 语句 + 图结构存储 + 支持图查询

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// 文件节点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileNode {
    /// 文件路径（相对项目根目录）
    pub path: PathBuf,
    /// 文件语言
    pub language: Language,
    /// 该文件定义的符号（函数/类/常量名）
    pub definitions: Vec<String>,
    /// 该文件导入的符号
    pub imports: Vec<ImportInfo>,
    /// 文件哈希（用于增量更新检测）
    pub content_hash: u64,
}

/// 编程语言
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    Unknown,
}

/// 导入信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportInfo {
    /// 导入的模块路径
    pub module_path: String,
    /// 导入的符号（None = 全部导入）
    pub symbols: Option<Vec<String>>,
    /// 是否为相对导入
    pub is_relative: bool,
}

/// 依赖边
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyEdge {
    /// 源文件
    pub from: PathBuf,
    /// 目标文件
    pub to: PathBuf,
    /// 依赖类型
    pub dep_type: DependencyType,
}

/// 依赖类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DependencyType {
    /// 直接 import
    Direct,
    /// 重导出（re-export）
    ReExport,
    /// 类型依赖（TypeScript type import, Rust trait impl）
    TypeOnly,
}

/// Repo Map — 文件级依赖图
pub struct RepoMap {
    /// 文件节点表
    files: HashMap<PathBuf, FileNode>,
    /// 邻接表：文件 → 其依赖的文件列表
    dependencies: HashMap<PathBuf, Vec<DependencyEdge>>,
    /// 反向邻接表：文件 → 依赖该文件的文件列表
    dependents: HashMap<PathBuf, Vec<DependencyEdge>>,
    /// 项目根目录
    root: PathBuf,
}

impl RepoMap {
    /// 创建空的 RepoMap
    pub fn new(root: PathBuf) -> Self {
        Self {
            files: HashMap::new(),
            dependencies: HashMap::new(),
            dependents: HashMap::new(),
            root,
        }
    }

    /// 添加文件节点
    pub fn add_file(&mut self, node: FileNode) {
        let path = node.path.clone();
        // 先清理旧的边
        self.remove_file_edges(&path);
        // 添加新节点
        self.files.insert(path.clone(), node);
    }

    /// 添加依赖边
    pub fn add_dependency(&mut self, edge: DependencyEdge) {
        self.dependencies
            .entry(edge.from.clone())
            .or_default()
            .push(edge.clone());
        self.dependents
            .entry(edge.to.clone())
            .or_default()
            .push(edge);
    }

    /// 移除文件（及其所有边）
    pub fn remove_file(&mut self, path: &Path) {
        self.remove_file_edges(path);
        self.files.remove(path);
    }

    /// 获取文件节点
    pub fn get_file(&self, path: &Path) -> Option<&FileNode> {
        self.files.get(path)
    }

    /// 获取文件的直接依赖
    pub fn dependencies_of(&self, path: &Path) -> Vec<&DependencyEdge> {
        self.dependencies
            .get(path)
            .map(|edges| edges.iter().collect())
            .unwrap_or_default()
    }

    /// 获取文件的直接依赖者（谁依赖了这个文件）
    pub fn dependents_of(&self, path: &Path) -> Vec<&DependencyEdge> {
        self.dependents
            .get(path)
            .map(|edges| edges.iter().collect())
            .unwrap_or_default()
    }

    /// 获取所有依赖文件（传递闭包，BFS）
    pub fn all_dependencies(&self, path: &Path) -> HashSet<PathBuf> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(path.to_path_buf());

        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(edges) = self.dependencies.get(&current) {
                for edge in edges {
                    if !visited.contains(&edge.to) {
                        queue.push_back(edge.to.clone());
                    }
                }
            }
        }

        visited.remove(path);
        visited
    }

    /// 获取所有依赖者（反向传递闭包，BFS）
    /// 即"修改这个文件会影响哪些文件"
    pub fn all_dependents(&self, path: &Path) -> HashSet<PathBuf> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(path.to_path_buf());

        while let Some(current) = queue.pop_front() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(edges) = self.dependents.get(&current) {
                for edge in edges {
                    if !visited.contains(&edge.from) {
                        queue.push_back(edge.from.clone());
                    }
                }
            }
        }

        visited.remove(path);
        visited
    }

    /// 影响范围分析：修改指定文件后，受影响的文件列表
    pub fn impact_analysis(&self, changed_files: &[PathBuf]) -> HashSet<PathBuf> {
        let mut impacted = HashSet::new();
        for path in changed_files {
            impacted.extend(self.all_dependents(path));
        }
        impacted
    }

    /// 查找两个文件之间的最短依赖路径（BFS）
    pub fn find_dependency_path(&self, from: &Path, to: &Path) -> Option<Vec<PathBuf>> {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back((from.to_path_buf(), vec![from.to_path_buf()]));

        while let Some((current, path)) = queue.pop_front() {
            if &current == to {
                return Some(path);
            }
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            if let Some(edges) = self.dependencies.get(&current) {
                for edge in edges {
                    if !visited.contains(&edge.to) {
                        let mut new_path = path.clone();
                        new_path.push(edge.to.clone());
                        queue.push_back((edge.to.clone(), new_path));
                    }
                }
            }
        }

        None
    }

    /// 生成 repo-map 摘要（给 LLM 用的文本格式，类似 Aider）
    ///
    /// 格式：每个文件一行，列出其定义的符号和依赖
    pub fn to_summary(&self) -> String {
        let mut lines: Vec<String> = self.files.keys().map(|p| p.to_string_lossy().to_string()).collect();
        lines.sort();

        let mut summary = String::new();
        for path_str in &lines {
            let path = PathBuf::from(path_str);
            if let Some(node) = self.files.get(&path) {
                let defs = node.definitions.join(", ");
                let deps: Vec<String> = self.dependencies_of(&path)
                    .iter()
                    .map(|e| e.to.to_string_lossy().to_string())
                    .collect();
                let dep_str = if deps.is_empty() {
                    String::new()
                } else {
                    format!(" → {}", deps.join(", "))
                };
                if defs.is_empty() {
                    summary.push_str(&format!("{}{}\n", path_str, dep_str));
                } else {
                    summary.push_str(&format!("{}: {}{}\n", path_str, defs, dep_str));
                }
            }
        }
        summary
    }

    /// 获取文件数量
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 获取边数量
    pub fn edge_count(&self) -> usize {
        self.dependencies.values().map(|v| v.len()).sum()
    }

    /// 查找定义了指定符号的文件
    pub fn find_definition(&self, symbol: &str) -> Vec<&FileNode> {
        self.files
            .values()
            .filter(|f| f.definitions.contains(&symbol.to_string()))
            .collect()
    }

    /// 根据语言解析 import 语句并构建依赖边
    pub fn resolve_dependencies(&mut self) {
        let files: Vec<FileNode> = self.files.values().cloned().collect();
        for file in &files {
            let deps = Self::parse_imports(file, &self.root);
            for dep in deps {
                self.add_dependency(DependencyEdge {
                    from: file.path.clone(),
                    to: dep,
                    dep_type: DependencyType::Direct,
                });
            }
        }
    }

    // ---- 内部方法 ----

    fn remove_file_edges(&mut self, path: &Path) {
        // 移除以该文件为源的所有边
        self.dependencies.remove(path);
        // 移除以该文件为目标的边
        for edges in self.dependencies.values_mut() {
            edges.retain(|e| e.to != path);
        }
        // 移除反向索引
        self.dependents.remove(path);
        for edges in self.dependents.values_mut() {
            edges.retain(|e| e.from != path);
        }
    }

    /// 解析文件的 import 语句，返回依赖的文件路径列表
    fn parse_imports(file: &FileNode, root: &Path) -> Vec<PathBuf> {
        let mut deps = Vec::new();
        for import in &file.imports {
            let resolved = Self::resolve_module_path(&import.module_path, &file.path, root, file.language);
            if let Some(path) = resolved {
                if !deps.contains(&path) {
                    deps.push(path);
                }
            }
        }
        deps
    }

    /// 将模块路径解析为文件路径
    fn resolve_module_path(module_path: &str, from_file: &Path, root: &Path, lang: Language) -> Option<PathBuf> {
        match lang {
            Language::Rust => {
                // Rust: crate::module → src/module.rs 或 src/module/mod.rs
                if module_path.starts_with("crate::") || module_path.starts_with("super::") || module_path.starts_with("self::") {
                    let relative = module_path.replace("::", "/").replace("crate/", "src/").replace("super/", "").replace("self/", "");
                    let candidates = [
                        format!("{}.rs", relative),
                        format!("{}/mod.rs", relative),
                    ];
                    for candidate in &candidates {
                        let path = root.join(candidate);
                        if path.exists() {
                            return Some(PathBuf::from(candidate));
                        }
                    }
                }
                None
            }
            Language::TypeScript | Language::JavaScript => {
                // TS/JS: relative import → 解析为文件路径
                if module_path.starts_with('.') {
                    let dir = from_file.parent()?;
                    let candidates = [
                        format!("{}.ts", module_path),
                        format!("{}.tsx", module_path),
                        format!("{}.js", module_path),
                        format!("{}.jsx", module_path),
                        format!("{}/index.ts", module_path),
                        format!("{}/index.tsx", module_path),
                    ];
                    for candidate in &candidates {
                        let resolved = dir.join(candidate);
                        if resolved.exists() {
                            return Some(resolved.strip_prefix(root).unwrap_or(&resolved).to_path_buf());
                        }
                    }
                }
                None
            }
            Language::Python => {
                // Python: from package.module import → package/module.py
                if !module_path.starts_with('.') {
                    let relative = module_path.replace('.', "/");
                    let candidates = [
                        format!("{}.py", relative),
                        format!("{}/__init__.py", relative),
                    ];
                    for candidate in &candidates {
                        let path = root.join(candidate);
                        if path.exists() {
                            return Some(PathBuf::from(candidate));
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }
}

/// Import 语句正则解析器
pub struct ImportParser;

impl ImportParser {
    /// 从源代码中提取 import 信息
    pub fn parse(source: &str, language: Language) -> Vec<ImportInfo> {
        match language {
            Language::Rust => Self::parse_rust(source),
            Language::TypeScript | Language::JavaScript => Self::parse_typescript(source),
            Language::Python => Self::parse_python(source),
            Language::Go => Self::parse_go(source),
            _ => Vec::new(),
        }
    }

    fn parse_rust(source: &str) -> Vec<ImportInfo> {
        let mut imports = Vec::new();
        // use crate::module::Symbol;
        // use crate::module::{A, B, C};
        for line in source.lines() {
            let trimmed = line.trim();
            if !trimmed.starts_with("use ") {
                continue;
            }
            let use_stmt = trimmed.trim_start_matches("use ").trim_end_matches(';').trim();
            if let Some((path, symbols)) = use_stmt.split_once("::") {
                let full_path = path.to_string();
                if let Some(syms) = symbols.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    let symbols: Vec<String> = syms.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    imports.push(ImportInfo {
                        module_path: full_path,
                        symbols: if symbols.is_empty() { None } else { Some(symbols) },
                        is_relative: path == "crate" || path == "super" || path == "self",
                    });
                } else {
                    // use crate::module::Symbol (单个符号)
                    let parts: Vec<&str> = use_stmt.split("::").collect();
                    if parts.len() >= 2 {
                        let module = parts[..parts.len() - 1].join("::");
                        let symbol = parts[parts.len() - 1].to_string();
                        imports.push(ImportInfo {
                            module_path: module,
                            symbols: Some(vec![symbol]),
                            is_relative: parts[0] == "crate" || parts[0] == "super" || parts[0] == "self",
                        });
                    }
                }
            }
        }
        imports
    }

    fn parse_typescript(source: &str) -> Vec<ImportInfo> {
        let mut imports = Vec::new();
        for line in source.lines() {
            let trimmed = line.trim();
            // import ... from 'module'
            // import { A, B } from 'module'
            // import type { T } from 'module'
            if !trimmed.starts_with("import ") {
                continue;
            }
            if let Some(from_idx) = trimmed.find(" from ") {
                let after_from = &trimmed[from_idx + 6..].trim();
                let module = after_from.trim_start_matches(&['\'', '"'][..])
                    .trim_end_matches(&['\'', '"', ';'][..])
                    .to_string();
                let is_relative = module.starts_with('.');
                imports.push(ImportInfo {
                    module_path: module,
                    symbols: None, // TODO: 解析具体符号
                    is_relative,
                });
            }
        }
        imports
    }

    fn parse_python(source: &str) -> Vec<ImportInfo> {
        let mut imports = Vec::new();
        for line in source.lines() {
            let trimmed = line.trim();
            // from package.module import Symbol
            // import package.module
            if trimmed.starts_with("from ") {
                if let Some(import_idx) = trimmed.find(" import ") {
                    let module = trimmed[5..import_idx].trim().to_string();
                    let syms_str = &trimmed[import_idx + 8..];
                    let symbols: Vec<String> = syms_str.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    imports.push(ImportInfo {
                        module_path: module,
                        symbols: if symbols.is_empty() { None } else { Some(symbols) },
                        is_relative: trimmed[5..6].starts_with('.'),
                    });
                }
            } else if trimmed.starts_with("import ") {
                let module = trimmed[7..].trim().trim_end_matches(';').to_string();
                imports.push(ImportInfo {
                    module_path: module,
                    symbols: None,
                    is_relative: false,
                });
            }
        }
        imports
    }

    fn parse_go(source: &str) -> Vec<ImportInfo> {
        let mut imports = Vec::new();
        let mut in_import_block = false;
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed == "import (" {
                in_import_block = true;
                continue;
            }
            if trimmed == ")" {
                in_import_block = false;
                continue;
            }
            if in_import_block || trimmed.starts_with("import ") {
                let import_line = if in_import_block {
                    trimmed
                } else {
                    trimmed.trim_start_matches("import ")
                };
                let module = import_line.trim_start_matches(&['"', '`'][..])
                    .trim_end_matches(&['"', '`'][..])
                    .trim()
                    .to_string();
                if !module.is_empty() {
                    imports.push(ImportInfo {
                        module_path: module,
                        symbols: None,
                        is_relative: false,
                    });
                }
            }
        }
        imports
    }
}

/// 语言检测（基于文件扩展名）
pub fn detect_language(path: &Path) -> Language {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => Language::Rust,
        Some("ts") | Some("tsx") => Language::TypeScript,
        Some("js") | Some("jsx") => Language::JavaScript,
        Some("py") => Language::Python,
        Some("go") => Language::Go,
        Some("java") => Language::Java,
        Some("c") | Some("h") => Language::C,
        Some("cpp") | Some("cc") | Some("hpp") => Language::Cpp,
        _ => Language::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_repo_map_basic() {
        let mut map = RepoMap::new(PathBuf::from("/project"));

        map.add_file(FileNode {
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            definitions: vec!["main".to_string()],
            imports: vec![ImportInfo {
                module_path: "crate::config".to_string(),
                symbols: Some(vec!["Config".to_string()]),
                is_relative: true,
            }],
            content_hash: 0,
        });

        map.add_file(FileNode {
            path: PathBuf::from("src/config.rs"),
            language: Language::Rust,
            definitions: vec!["Config".to_string()],
            imports: vec![],
            content_hash: 0,
        });

        map.add_dependency(DependencyEdge {
            from: PathBuf::from("src/main.rs"),
            to: PathBuf::from("src/config.rs"),
            dep_type: DependencyType::Direct,
        });

        assert_eq!(map.file_count(), 2);
        assert_eq!(map.edge_count(), 1);

        // main 依赖 config
        let deps = map.dependencies_of(Path::new("src/main.rs"));
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].to, PathBuf::from("src/config.rs"));

        // config 被 main 依赖
        let dependents = map.dependents_of(Path::new("src/config.rs"));
        assert_eq!(dependents.len(), 1);

        // 影响分析
        let impacted = map.impact_analysis(&[PathBuf::from("src/config.rs")]);
        assert!(impacted.contains(&PathBuf::from("src/main.rs")));
    }

    #[test]
    fn test_all_dependencies_transitive() {
        let mut map = RepoMap::new(PathBuf::from("/project"));

        map.add_file(FileNode {
            path: PathBuf::from("a.rs"), language: Language::Rust,
            definitions: vec![], imports: vec![], content_hash: 0,
        });
        map.add_file(FileNode {
            path: PathBuf::from("b.rs"), language: Language::Rust,
            definitions: vec![], imports: vec![], content_hash: 0,
        });
        map.add_file(FileNode {
            path: PathBuf::from("c.rs"), language: Language::Rust,
            definitions: vec![], imports: vec![], content_hash: 0,
        });

        // a → b → c
        map.add_dependency(DependencyEdge {
            from: PathBuf::from("a.rs"), to: PathBuf::from("b.rs"), dep_type: DependencyType::Direct,
        });
        map.add_dependency(DependencyEdge {
            from: PathBuf::from("b.rs"), to: PathBuf::from("c.rs"), dep_type: DependencyType::Direct,
        });

        let all_deps = map.all_dependencies(Path::new("a.rs"));
        assert!(all_deps.contains(&PathBuf::from("b.rs")));
        assert!(all_deps.contains(&PathBuf::from("c.rs")));
    }

    #[test]
    fn test_parse_rust_imports() {
        let source = r#"
use std::collections::HashMap;
use crate::config::Config;
use crate::models::{User, Post};
"#;
        let imports = ImportParser::parse(source, Language::Rust);
        assert!(imports.len() >= 2);
    }

    #[test]
    fn test_parse_typescript_imports() {
        let source = r#"
import { useState, useEffect } from 'react';
import axios from 'axios';
import type { Config } from './config';
"#;
        let imports = ImportParser::parse(source, Language::TypeScript);
        assert_eq!(imports.len(), 3);
    }

    #[test]
    fn test_parse_python_imports() {
        let source = r#"
from typing import List, Dict
import os
from .config import Settings
"#;
        let imports = ImportParser::parse(source, Language::Python);
        assert_eq!(imports.len(), 3);
    }

    #[test]
    fn test_find_definition() {
        let mut map = RepoMap::new(PathBuf::from("/project"));
        map.add_file(FileNode {
            path: PathBuf::from("src/user.rs"), language: Language::Rust,
            definitions: vec!["User".to_string(), "create_user".to_string()],
            imports: vec![], content_hash: 0,
        });

        let found = map.find_definition("User");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, PathBuf::from("src/user.rs"));
    }

    #[test]
    fn test_to_summary() {
        let mut map = RepoMap::new(PathBuf::from("/project"));
        map.add_file(FileNode {
            path: PathBuf::from("src/main.rs"), language: Language::Rust,
            definitions: vec!["main".to_string()], imports: vec![], content_hash: 0,
        });
        map.add_file(FileNode {
            path: PathBuf::from("src/config.rs"), language: Language::Rust,
            definitions: vec!["Config".to_string()], imports: vec![], content_hash: 0,
        });
        map.add_dependency(DependencyEdge {
            from: PathBuf::from("src/main.rs"), to: PathBuf::from("src/config.rs"), dep_type: DependencyType::Direct,
        });

        let summary = map.to_summary();
        assert!(summary.contains("main"));
        assert!(summary.contains("Config"));
    }
}
