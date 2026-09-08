//! 项目上下文工具 — 发现并加载项目规范文档（CCODE.md / CLAUDE.md / AGENTS.md）
//!
//! 对标 codex `agents_md.rs` 与 Claude Code 的 CLAUDE.md 发现规则：
//!
//! 1. **向上遍历**：从工作目录向上找到项目根（含 `.git` 标记的目录），
//!    收集 **项目根到工作目录之间每一层** 的规范文档，不越过项目根。
//!    没有 `.git` 标记时退化为「只看工作目录」，不会一路走到文件系统根。
//! 2. **文件名优先级**：每一层按 `CCODE.md` → `CLAUDE.md` → `AGENTS.md` 取**第一个存在**的，
//!    同一目录不会重复注入内容相同的多份文档。
//! 3. **注入顺序**：项目根在前、工作目录在后 —— 越靠后越具体，子目录规范覆盖父目录。
//! 4. **字节上限**：总量默认 32KB（与 codex `project_doc_max_bytes` 一致）。
//!    预算按「离工作目录越近越优先」分配，超出部分截断并提示模型用 `read_file` 看全文，
//!    避免一份 200KB 的规范原文塞满上下文。

use std::path::{Path, PathBuf};

/// 每一层目录内的候选文件名，按优先级从高到低。
///
/// 同一目录只取第一个存在的：仓库里常见 `CLAUDE.md` 与 `AGENTS.md` 互为副本，
/// 全读会把同样的内容注入两遍。
pub const PROJECT_DOC_FILENAMES: [&str; 3] = ["CCODE.md", "CLAUDE.md", "AGENTS.md"];

/// 注入上下文的总字节上限（与 codex `project_doc_max_bytes` 默认值一致）。
pub const DEFAULT_MAX_BYTES: usize = 32 * 1024;

/// 单个文件允许读入内存的上限 —— 超过则只读前 `MAX_FILE_BYTES` 字节。
///
/// 与总预算分开：总预算负责注入量，这一条负责「不要为了一份被误提交的巨型文件
/// 先把它整份读进内存」。
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// 发现到的一份项目规范文档
#[derive(Debug, Clone)]
pub struct ProjectDoc {
    /// 文件路径
    pub path: PathBuf,
    /// 文件内容（可能已按 `MAX_FILE_BYTES` 截断）
    pub content: String,
    /// 目录深度（越大越靠近工作目录，优先级越高）
    pub depth: usize,
}

/// 项目上下文（从 CCODE.md / CLAUDE.md / AGENTS.md 加载）
#[derive(Debug, Clone, Default)]
pub struct ProjectContext {
    /// 格式化后的注入内容（不含 `[项目规范]` 头）
    pub content: String,
    /// 是否已加载
    pub loaded: bool,
    /// 实际参与注入的文档路径（root → cwd 顺序）
    pub doc_paths: Vec<PathBuf>,
    /// 是否因预算不足发生截断
    pub truncated: bool,
}

impl ProjectContext {
    /// 从工作目录加载项目上下文（默认 32KB 预算）
    pub fn load_from_dir(dir: &Path) -> Self {
        Self::load_with_limit(dir, DEFAULT_MAX_BYTES)
    }

    /// 从工作目录加载项目上下文，指定总字节预算
    pub fn load_with_limit(dir: &Path, max_bytes: usize) -> Self {
        let docs = discover_project_docs(dir);
        if docs.is_empty() {
            return Self::default();
        }

        let (content, doc_paths, truncated) = format_docs(&docs, max_bytes);
        if content.is_empty() {
            return Self::default();
        }

        tracing::info!(
            docs = doc_paths.len(),
            bytes = content.len(),
            truncated,
            "已加载项目上下文"
        );

        Self { content, loaded: true, doc_paths, truncated }
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

/// 定位项目根：从 `start` 向上找第一个含 `.git` 的目录
///
/// 返回 `None` 表示不在任何仓库里 —— 此时调用方只看 `start` 本身，
/// 不向上遍历（避免把 `$HOME` 甚至 `/` 下的无关文档拖进上下文）。
fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// 发现从项目根到工作目录之间每一层的项目规范文档
///
/// 返回顺序为 **root → cwd**（越靠后越具体）。
pub fn discover_project_docs(start_dir: &Path) -> Vec<ProjectDoc> {
    // 尽量用规范化路径：向上遍历依赖 parent()，`./sub` 这类相对路径会提前断链
    let start = std::fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());

    // 构造 root → cwd 的目录链
    let chain: Vec<PathBuf> = match find_project_root(&start) {
        Some(root) => {
            let mut chain = Vec::new();
            let mut current = Some(start.as_path());
            while let Some(dir) = current {
                chain.push(dir.to_path_buf());
                if dir == root {
                    break;
                }
                current = dir.parent();
            }
            chain.reverse(); // 收集时是 cwd → root，翻转成 root → cwd
            chain
        }
        // 不在仓库里：只看工作目录本身
        None => vec![start.clone()],
    };

    let mut docs = Vec::new();
    for (depth, dir) in chain.iter().enumerate() {
        // 每层只取第一个存在的候选文件名
        for name in PROJECT_DOC_FILENAMES {
            let path = dir.join(name);
            if !path.is_file() {
                continue;
            }
            match read_capped(&path) {
                Ok(content) if !content.trim().is_empty() => {
                    docs.push(ProjectDoc { path, content, depth });
                }
                Ok(_) => {
                    // 空文件：跳过但不再试其他候选名（该层已表态用这个文件）
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "读取项目规范失败");
                }
            }
            break;
        }
    }

    docs
}

/// 读取文件，超过 `MAX_FILE_BYTES` 时只读前面部分
fn read_capped(path: &Path) -> std::io::Result<String> {
    let len = std::fs::metadata(path)?.len();
    if len <= MAX_FILE_BYTES {
        return std::fs::read_to_string(path);
    }

    use std::io::Read;
    let mut buf = vec![0u8; MAX_FILE_BYTES as usize];
    let mut file = std::fs::File::open(path)?;
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    // 可能切在多字节字符中间，从末尾丢弃不完整序列
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 把发现的文档格式化为注入文本
///
/// 预算分配方向与注入方向相反：**分配从最深（最具体）的文档开始**，
/// 保证离工作目录最近的规范一定拿到额度；**注入按 root → cwd 顺序**，
/// 让更具体的规范出现在后面从而覆盖父目录。
fn format_docs(docs: &[ProjectDoc], max_bytes: usize) -> (String, Vec<PathBuf>, bool) {
    const HEADER: &str = "<project_docs>\n";
    const FOOTER: &str = "</project_docs>";
    const TRUNCATION_NOTE: &str = "\n[... 已截断，用 read_file 读取完整文件 ...]\n";
    const BUDGET_NOTE: &str = "[注意：部分项目规范因上下文预算被截断，可用 read_file 读取完整文件]\n";

    // 结构性开销（头尾标签 + 可能追加的整体截断提示）先从预算里扣掉，
    // 否则最终输出会超出 max_bytes
    let mut remaining =
        max_bytes.saturating_sub(HEADER.len() + FOOTER.len() + BUDGET_NOTE.len());
    let mut truncated = false;

    // 每份文档实际能注入多少字节：从最深的开始分配
    let mut allocated: Vec<usize> = vec![0; docs.len()];
    for idx in (0..docs.len()).rev() {
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let entry_header = doc_header(&docs[idx].path);
        // 头都放不下：这份整体跳过
        if entry_header.len() + TRUNCATION_NOTE.len() + 64 > remaining {
            truncated = true;
            continue;
        }
        let body_budget = remaining - entry_header.len() - 2; // 2 = 结尾两个换行
        let body = docs[idx].content.len();
        if body <= body_budget {
            allocated[idx] = body;
            remaining -= entry_header.len() + body + 2;
        } else {
            // 留出截断提示的空间
            let usable = body_budget.saturating_sub(TRUNCATION_NOTE.len());
            allocated[idx] = usable;
            remaining = 0;
            truncated = true;
        }
    }

    let mut out = String::from(HEADER);
    let mut paths = Vec::new();
    for (idx, doc) in docs.iter().enumerate() {
        let budget = allocated[idx];
        if budget == 0 {
            continue;
        }
        out.push_str(&doc_header(&doc.path));
        if budget >= doc.content.len() {
            out.push_str(&doc.content);
        } else {
            out.push_str(&doc.content[..floor_char_boundary(&doc.content, budget)]);
            out.push_str(TRUNCATION_NOTE);
        }
        out.push_str("\n\n");
        paths.push(doc.path.clone());
    }

    if paths.is_empty() {
        return (String::new(), Vec::new(), truncated);
    }

    if truncated {
        out.push_str(BUDGET_NOTE);
    }
    out.push_str(FOOTER);
    (out, paths, truncated)
}

fn doc_header(path: &Path) -> String {
    format!("## {}\n", path.display())
}

/// `str::floor_char_boundary` 的稳定版实现 —— 从 `idx` 向左找到最近的字符边界
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 建一个带 `.git` 标记的目录树，返回 tempdir
    fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join(".git")).unwrap();
        tmp
    }

    #[test]
    fn loads_ccode_md_from_working_dir() {
        let tmp = repo();
        fs::write(tmp.path().join("CCODE.md"), "# Test\nHello world").unwrap();

        let ctx = ProjectContext::load_from_dir(tmp.path());
        assert!(ctx.loaded);
        assert!(ctx.content.contains("Hello world"));
        assert!(ctx.system_prompt().unwrap().starts_with("[项目规范]"));
    }

    #[test]
    fn falls_back_to_claude_md_then_agents_md() {
        // 只有 CLAUDE.md
        let tmp = repo();
        fs::write(tmp.path().join("CLAUDE.md"), "claude rules").unwrap();
        assert!(ProjectContext::load_from_dir(tmp.path()).content.contains("claude rules"));

        // 只有 AGENTS.md —— 这条是 P0-2'：此前主线一个字都读不到
        let tmp2 = repo();
        fs::write(tmp2.path().join("AGENTS.md"), "agents rules").unwrap();
        let ctx = ProjectContext::load_from_dir(tmp2.path());
        assert!(ctx.loaded, "AGENTS.md 必须被主线消费");
        assert!(ctx.content.contains("agents rules"));
    }

    #[test]
    fn same_dir_takes_only_highest_priority_filename() {
        let tmp = repo();
        fs::write(tmp.path().join("CCODE.md"), "ccode wins").unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "claude loses").unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "agents loses").unwrap();

        let ctx = ProjectContext::load_from_dir(tmp.path());
        assert!(ctx.content.contains("ccode wins"));
        assert!(!ctx.content.contains("claude loses"));
        assert!(!ctx.content.contains("agents loses"));
        assert_eq!(ctx.doc_paths.len(), 1);
    }

    #[test]
    fn walks_up_to_git_root_and_orders_root_first() {
        // P0-3'：monorepo 子目录启动必须仍能读到项目根规范
        let tmp = repo();
        let sub = tmp.path().join("crates").join("thing");
        fs::create_dir_all(&sub).unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "ROOT RULES").unwrap();
        fs::write(sub.join("AGENTS.md"), "LEAF RULES").unwrap();

        let ctx = ProjectContext::load_from_dir(&sub);
        assert_eq!(ctx.doc_paths.len(), 2);
        let root_at = ctx.content.find("ROOT RULES").unwrap();
        let leaf_at = ctx.content.find("LEAF RULES").unwrap();
        assert!(root_at < leaf_at, "项目根规范应在前，子目录规范在后以覆盖父目录");
    }

    #[test]
    fn does_not_walk_past_git_root() {
        let outer = tempfile::tempdir().unwrap();
        fs::write(outer.path().join("AGENTS.md"), "OUTSIDE").unwrap();
        let inner = outer.path().join("repo");
        fs::create_dir_all(inner.join(".git")).unwrap();
        fs::write(inner.join("AGENTS.md"), "INSIDE").unwrap();

        let ctx = ProjectContext::load_from_dir(&inner);
        assert!(ctx.content.contains("INSIDE"));
        assert!(!ctx.content.contains("OUTSIDE"), "不得越过项目根向上");
    }

    #[test]
    fn no_repo_marker_means_no_upward_traversal() {
        let outer = tempfile::tempdir().unwrap();
        fs::write(outer.path().join("AGENTS.md"), "OUTSIDE").unwrap();
        let inner = outer.path().join("plain");
        fs::create_dir_all(&inner).unwrap();

        let ctx = ProjectContext::load_from_dir(&inner);
        assert!(!ctx.loaded, "无 .git 标记时不应向上捞父目录文档");
    }

    #[test]
    fn empty_dir_yields_unloaded_context() {
        let tmp = repo();
        let ctx = ProjectContext::load_from_dir(tmp.path());
        assert!(!ctx.loaded);
        assert!(ctx.system_prompt().is_none());
    }

    #[test]
    fn empty_file_is_not_treated_as_loaded() {
        let tmp = repo();
        fs::write(tmp.path().join("CCODE.md"), "   \n\n").unwrap();
        assert!(!ProjectContext::load_from_dir(tmp.path()).loaded);
    }

    #[test]
    fn respects_byte_budget_and_flags_truncation() {
        let tmp = repo();
        let huge = "x".repeat(50_000);
        fs::write(tmp.path().join("CCODE.md"), &huge).unwrap();

        let ctx = ProjectContext::load_with_limit(tmp.path(), 4096);
        assert!(ctx.loaded);
        assert!(ctx.truncated);
        assert!(ctx.content.len() <= 4096, "实际 {} 字节", ctx.content.len());
        assert!(ctx.content.contains("read_file"));
    }

    #[test]
    fn budget_prefers_the_nearest_doc() {
        // 预算不够两份时，靠近工作目录的那份（更具体）必须保住
        let tmp = repo();
        let sub = tmp.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(tmp.path().join("CCODE.md"), "R".repeat(3000)).unwrap();
        fs::write(sub.join("CCODE.md"), "LEAFMARK".to_string() + &"L".repeat(500)).unwrap();

        let ctx = ProjectContext::load_with_limit(&sub, 1200);
        assert!(ctx.content.contains("LEAFMARK"), "最近的文档必须优先获得预算");
        assert!(ctx.truncated);
    }

    #[test]
    fn truncation_never_splits_a_multibyte_char() {
        let tmp = repo();
        // 全中文，任意字节截断点都有极高概率落在字符中间
        fs::write(tmp.path().join("CCODE.md"), "项目规范".repeat(2000)).unwrap();
        let ctx = ProjectContext::load_with_limit(tmp.path(), 2048);
        assert!(ctx.truncated);
        // content 本身是 String，能构造出来就说明没有切坏 UTF-8
        assert!(ctx.content.contains("项目规范"));
    }
}
