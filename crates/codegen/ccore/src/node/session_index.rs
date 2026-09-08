//! 会话存储的目录布局、发现、解析与回收
//!
//! 会话落在 `<sessions_root>/<session_id>/`，每个目录里：
//!
//! - `conversation.jsonl` —— **真相源**，每轮一行追加写，内容完整未压缩
//! - `meta.json` —— 启动时写一次：`{session_id, cwd, created_at}`
//! - `conversation.json` —— 退出时写一次的可读快照（**不要用它恢复**，见下）
//!
//! ## 为什么恢复只认 jsonl
//!
//! `conversation.json` 来自 `ShortTermMemory`，那里有 500 条上限的驱逐，
//! 且压缩会原地截断工具输出 —— 它是**双重降级**的。用它恢复会静默丢掉前期消息、
//! 且工具输出是压缩后的残缺版本。`conversation.jsonl` 才是完整原始流。
//!
//! ## 为什么用 per-session `meta.json` 而不是全局 index
//!
//! 全局追加索引需要一条与真实目录保持同步的写路径，一旦漂移（手删目录、
//! 写一半崩溃）列表就开始骗人。`meta.json` 与会话目录同生共死，
//! 枚举即真相，GC 就是删目录，没有需要维护的第二份状态。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 从哪个会话恢复
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeTarget {
    /// 当前工作目录下最近更新的一次会话（`--continue`）
    LastInCwd,
    /// 全局最近更新的一次会话（跨目录）
    LastAnywhere,
    /// 指定 session id（`--resume <ID>`）
    Id(String),
}

/// 会话元数据（启动时写一次）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    /// 会话创建时的工作目录（`--continue` 靠它匹配）
    pub cwd: String,
    pub created_at: String,
}

/// 列表用的会话摘要
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub meta: SessionMeta,
    /// `conversation.jsonl` 的 mtime —— 排序依据
    pub updated_at: SystemTime,
    /// jsonl 里的行数
    pub entry_count: usize,
    /// 第一条 user 消息的摘要，供人辨认是哪个会话
    pub first_user_message: Option<String>,
}

/// 一条从 jsonl 复原出来的消息
#[derive(Debug, Clone)]
pub struct RestoredMessage {
    pub role: String,
    pub content: String,
    pub token_count: u32,
    pub is_tool_call: bool,
}

/// 会话存储根目录
///
/// `HOME` 直读会在 Windows 上静默降级成 `"."`，于是每个项目目录里都长出一个
/// `.ccode/sessions/` 并可能被提交进 git。这里补 `USERPROFILE` 回退。
pub fn sessions_root() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ccode")
        .join("sessions")
}

fn home_dir() -> Option<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
    }
    // Windows 上偶有只给 HOMEDRIVE+HOMEPATH 的环境
    match (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        (Ok(d), Ok(p)) if !d.is_empty() && !p.is_empty() => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

/// 校验 session id 可以安全地当作路径分量
///
/// id 来自命令行，直接 `root.join(id)` 会让 `--resume ../../etc` 读到仓外文件。
pub fn validate_session_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() {
        anyhow::bail!("会话 ID 为空");
    }
    if id.len() > 128 {
        anyhow::bail!("会话 ID 过长（{} 字符，上限 128）", id.len());
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!(
            "会话 ID `{}` 含非法字符（只允许字母、数字、`-`、`_`）。\
             用 `--resume` 不带参数可列出可选会话",
            id
        );
    }
    Ok(())
}

/// 写入会话 `meta.json`（幂等：恢复已有会话时保留原始内容不覆盖）
///
/// `cwd` 会被规范化后存盘：CLI 默认传的是 `"."`，原样存下来既没法显示给用户看，
/// 也没法让 `--continue` 从别的目录正确匹配。
pub fn write_meta(root: &Path, session_id: &str, cwd: &str) -> std::io::Result<()> {
    let dir = root.join(session_id);
    std::fs::create_dir_all(&dir)?;
    // 已有 meta 时保留原始 created_at / cwd，只在缺失时写
    let path = dir.join("meta.json");
    if path.exists() {
        return Ok(());
    }
    let meta = SessionMeta {
        session_id: session_id.to_string(),
        cwd: canonical_string(cwd),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, json)
}

/// 枚举所有会话，按最近更新倒序
pub fn list_sessions(root: &Path) -> Vec<SessionSummary> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return out, // 根目录不存在 = 还没有任何会话
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let jsonl = dir.join("conversation.jsonl");
        if !jsonl.is_file() {
            continue; // 只有空目录，没有可恢复内容
        }

        let session_id = match dir.file_name().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        // meta.json 缺失（旧会话 / 手工目录）时仍然可列出，cwd 记为未知
        let meta = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<SessionMeta>(&s).ok())
            .unwrap_or(SessionMeta {
                session_id: session_id.clone(),
                cwd: String::new(),
                created_at: String::new(),
            });

        let updated_at = std::fs::metadata(&jsonl)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let (entry_count, first_user_message) = scan_jsonl_summary(&jsonl);

        out.push(SessionSummary { meta, updated_at, entry_count, first_user_message });
    }

    out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    out
}

/// 扫一遍 jsonl 拿行数与第一条 user 消息
fn scan_jsonl_summary(path: &Path) -> (usize, Option<String>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return (0, None),
    };
    let mut count = 0;
    let mut first_user = None;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        if first_user.is_none() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                let is_user = v.get("entry_type").and_then(|t| t.as_str()) == Some("user")
                    || v.get("role").and_then(|t| t.as_str()) == Some("user");
                if is_user {
                    if let Some(text) = v.get("content").and_then(|c| c.as_str()) {
                        first_user = Some(summarize(text, 80));
                    }
                }
            }
        }
    }
    (count, first_user)
}

/// 单行摘要：压掉换行，按**字符**（不是字节）截断
fn summarize(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        flat
    } else {
        let head: String = flat.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

/// 把 `ResumeTarget` 解析成具体 session id
pub fn resolve(root: &Path, target: &ResumeTarget, cwd: &str) -> anyhow::Result<String> {
    match target {
        ResumeTarget::Id(id) => {
            validate_session_id(id)?;
            let jsonl = root.join(id).join("conversation.jsonl");
            if !jsonl.is_file() {
                anyhow::bail!(
                    "会话 `{}` 不存在或没有可恢复内容（找不到 {}）。\
                     用 `--resume` 不带参数可列出可选会话",
                    id,
                    jsonl.display()
                );
            }
            Ok(id.clone())
        }
        ResumeTarget::LastInCwd => {
            let sessions = list_sessions(root);
            // 归一化再比：cwd 可能一次是符号链接、一次是真实路径
            let want = canonical_string(cwd);
            sessions
                .iter()
                .find(|s| !s.meta.cwd.is_empty() && canonical_string(&s.meta.cwd) == want)
                .map(|s| s.meta.session_id.clone())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "当前目录 `{}` 下没有可恢复的会话。\
                         用 `--resume` 不带参数可列出全部会话，或 `--resume <ID>` 指定一个",
                        cwd
                    )
                })
        }
        ResumeTarget::LastAnywhere => list_sessions(root)
            .first()
            .map(|s| s.meta.session_id.clone())
            .ok_or_else(|| anyhow::anyhow!("还没有任何已保存的会话")),
    }
}

fn canonical_string(p: &str) -> String {
    std::fs::canonicalize(p)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string())
}

/// 从 jsonl 复原消息序列
///
/// 只认 `conversation.jsonl`（理由见模块头）。缺失即报错，**不静默回落到
/// 降级的 `conversation.json`** —— 那会让用户以为恢复成功而实际丢了消息。
pub fn read_transcript(root: &Path, session_id: &str) -> anyhow::Result<Vec<RestoredMessage>> {
    validate_session_id(session_id)?;
    let path = root.join(session_id).join("conversation.jsonl");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("读取会话记录 {} 失败：{}", path.display(), e))?;

    let mut out = Vec::new();
    let mut bad_lines = 0usize;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                bad_lines += 1;
                continue;
            }
        };
        let entry_type = v.get("entry_type").and_then(|t| t.as_str()).unwrap_or("");
        let content_text = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if content_text.is_empty() {
            continue;
        }
        let token_count = v
            .get("token_count")
            .and_then(|t| t.as_u64())
            .unwrap_or((content_text.len() / 4) as u64) as u32;

        let (role, is_tool_call) = match entry_type {
            "user" | "assistant" | "system" => (
                v.get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or(entry_type)
                    .to_string(),
                false,
            ),
            "tool_call" | "tool_result" => ("tool".to_string(), true),
            // 未知类型：跳过而不是猜，避免把内部事件当成对话喂给模型
            _ => continue,
        };

        out.push(RestoredMessage { role, content: content_text.to_string(), token_count, is_tool_call });
    }

    if bad_lines > 0 {
        tracing::warn!(
            target: "ccore::session",
            bad_lines,
            path = %path.display(),
            "会话记录中有无法解析的行，已跳过"
        );
    }

    Ok(out)
}

/// 清理旧会话，只保留最近 `keep` 个
///
/// 返回删除的数量。`~/.ccode/sessions/` 此前无任何清理路径，重度使用数月后
/// 会积起数千个 uuid 目录，用户无从判断哪个能删。
pub fn gc(root: &Path, keep: usize) -> usize {
    let sessions = list_sessions(root);
    if sessions.len() <= keep {
        return 0;
    }
    let mut removed = 0;
    for s in &sessions[keep..] {
        let dir = root.join(&s.meta.session_id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(
                target: "ccore::session",
                path = %dir.display(),
                error = %e,
                "清理旧会话失败"
            ),
        }
    }
    if removed > 0 {
        tracing::info!(target: "ccore::session", removed, keep, "已清理旧会话");
    }
    removed
}

/// 默认保留的会话数
pub const DEFAULT_GC_KEEP: usize = 100;

/// 渲染 `--resume`（不带参数）/ `--list-sessions` 的人类可读列表
pub fn render_session_list(sessions: &[SessionSummary]) -> String {
    if sessions.is_empty() {
        return "没有已保存的会话。\n（会话在 ~/.ccode/sessions/ 下，每次对话自动落盘）".into();
    }

    let mut out = format!("已保存 {} 个会话（最近的在前）：\n\n", sessions.len());
    for s in sessions {
        out.push_str(&format!("  {}\n", s.meta.session_id));
        let cwd = if s.meta.cwd.is_empty() { "(未知目录)" } else { &s.meta.cwd };
        out.push_str(&format!("    目录: {}\n", cwd));
        out.push_str(&format!("    条目: {}\n", s.entry_count));
        if let Some(msg) = &s.first_user_message {
            out.push_str(&format!("    首条: {}\n", msg));
        }
        out.push('\n');
    }
    out.push_str("恢复：ccode --resume <ID>\n继续当前目录最近一次：ccode --continue\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 造一个会话目录
    fn make_session(root: &Path, id: &str, cwd: &str, lines: &[&str]) {
        let dir = root.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("meta.json"),
            serde_json::to_string(&SessionMeta {
                session_id: id.into(),
                cwd: cwd.into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .unwrap(),
        )
        .unwrap();
        fs::write(dir.join("conversation.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    fn user_line(text: &str) -> String {
        format!(
            r#"{{"entry_type":"user","role":"user","content":"{}","token_count":5,"ts":"2026-01-01T00:00:00Z"}}"#,
            text
        )
    }

    #[test]
    fn rejects_path_traversal_in_session_id() {
        // 没有这条校验，`--resume ../../etc` 就能读到仓外文件
        assert!(validate_session_id("../../etc").is_err());
        assert!(validate_session_id("a/b").is_err());
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(&"x".repeat(200)).is_err());
        assert!(validate_session_id("9f2c-4a11_ab").is_ok());
    }

    #[test]
    fn lists_sessions_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_session(root, "old", "/proj", &[&user_line("first task")]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        make_session(root, "new", "/proj", &[&user_line("second task")]);

        let list = list_sessions(root);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].meta.session_id, "new");
        assert_eq!(list[0].first_user_message.as_deref(), Some("second task"));
        assert_eq!(list[0].entry_count, 1);
    }

    #[test]
    fn ignores_dirs_without_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("empty-session")).unwrap();
        assert!(list_sessions(tmp.path()).is_empty());
    }

    #[test]
    fn lists_sessions_missing_meta_as_unknown_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("legacy");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("conversation.jsonl"), user_line("hi") + "\n").unwrap();

        let list = list_sessions(tmp.path());
        assert_eq!(list.len(), 1, "旧会话没有 meta.json，仍应可列出");
        assert!(list[0].meta.cwd.is_empty());
    }

    #[test]
    fn missing_root_lists_nothing_instead_of_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_sessions(&tmp.path().join("never-created")).is_empty());
    }

    #[test]
    fn resolves_last_in_cwd_by_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_session(root, "other", "/somewhere-else", &[&user_line("x")]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        make_session(root, "mine", "/my/proj", &[&user_line("y")]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        // 更新的会话属于别的目录 —— --continue 不该拿它
        make_session(root, "newest-elsewhere", "/somewhere-else", &[&user_line("z")]);

        let id = resolve(root, &ResumeTarget::LastInCwd, "/my/proj").unwrap();
        assert_eq!(id, "mine");

        let anywhere = resolve(root, &ResumeTarget::LastAnywhere, "/my/proj").unwrap();
        assert_eq!(anywhere, "newest-elsewhere");
    }

    #[test]
    fn last_in_cwd_errors_with_actionable_message() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "s1", "/elsewhere", &[&user_line("x")]);
        let err = resolve(tmp.path(), &ResumeTarget::LastInCwd, "/my/proj").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/my/proj"));
        assert!(msg.contains("--resume"), "错误信息要给出下一步怎么做");
    }

    #[test]
    fn resolve_id_rejects_unknown_session() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resolve(tmp.path(), &ResumeTarget::Id("nope".into()), "/x").unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn reads_transcript_roles_and_tool_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        make_session(
            root,
            "s1",
            "/p",
            &[
                &user_line("build it"),
                r#"{"entry_type":"assistant","role":"assistant","content":"ok","token_count":2,"ts":"t"}"#,
                r#"{"entry_type":"tool_result","role":"tool","content":"out","tool_name":"bash","token_count":9,"ts":"t"}"#,
                r#"{"entry_type":"heartbeat","ts":"t"}"#,
                "not json at all",
            ],
        );

        let msgs = read_transcript(root, "s1").unwrap();
        assert_eq!(msgs.len(), 3, "未知类型与坏行都应跳过");
        assert_eq!(msgs[0].role, "user");
        assert!(!msgs[0].is_tool_call);
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[2].role, "tool");
        assert!(msgs[2].is_tool_call);
        assert_eq!(msgs[2].token_count, 9);
    }

    #[test]
    fn read_transcript_does_not_fall_back_to_degraded_json() {
        // P1-16：conversation.json 是双重降级的（500 条驱逐 + 压缩后截断），
        // 缺 jsonl 时必须报错而不是拿它顶替
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s1");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("conversation.json"), r#"{"messages":[]}"#).unwrap();

        assert!(read_transcript(tmp.path(), "s1").is_err());
    }

    #[test]
    fn read_transcript_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_transcript(tmp.path(), "../secrets").is_err());
    }

    #[test]
    fn gc_keeps_newest_and_removes_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for i in 0..5 {
            make_session(root, &format!("s{i}"), "/p", &[&user_line("x")]);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert_eq!(gc(root, 2), 3);
        let left = list_sessions(root);
        assert_eq!(left.len(), 2);
        assert_eq!(left[0].meta.session_id, "s4");
        assert_eq!(left[1].meta.session_id, "s3");
    }

    #[test]
    fn gc_is_noop_under_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "s0", "/p", &[&user_line("x")]);
        assert_eq!(gc(tmp.path(), 10), 0);
        assert_eq!(list_sessions(tmp.path()).len(), 1);
    }

    #[test]
    fn write_meta_preserves_original_created_at() {
        let tmp = tempfile::tempdir().unwrap();
        write_meta(tmp.path(), "s1", "/first").unwrap();
        let first = fs::read_to_string(tmp.path().join("s1/meta.json")).unwrap();
        // 恢复同一会话时再写一次，不应把 cwd/created_at 覆盖掉
        write_meta(tmp.path(), "s1", "/second").unwrap();
        let second = fs::read_to_string(tmp.path().join("s1/meta.json")).unwrap();
        assert_eq!(first, second);
        assert!(first.contains("/first"));
    }

    #[test]
    fn summarize_truncates_on_char_boundary() {
        let s = summarize(&"项目规范".repeat(40), 10);
        assert_eq!(s.chars().count(), 11); // 10 + 省略号
        assert!(s.ends_with('…'));
    }

    #[test]
    fn summarize_flattens_newlines() {
        assert_eq!(summarize("a\n\nb\tc", 80), "a b c");
    }

    #[test]
    fn render_list_is_actionable_when_empty() {
        assert!(render_session_list(&[]).contains("没有已保存的会话"));
    }

    #[test]
    fn render_list_shows_ids_and_hint() {
        let tmp = tempfile::tempdir().unwrap();
        make_session(tmp.path(), "abc123", "/proj", &[&user_line("fix the parser")]);
        let out = render_session_list(&list_sessions(tmp.path()));
        assert!(out.contains("abc123"));
        assert!(out.contains("/proj"));
        assert!(out.contains("fix the parser"));
        assert!(out.contains("--resume"));
    }
}
