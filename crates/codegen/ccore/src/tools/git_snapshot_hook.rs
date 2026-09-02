//! Git 快照钩子 - Write/Edit 工具执行前自动创建 git 快照
//!
//! 作为 PreExecuteHook 注册到 ToolBridge：
//! - 仅对 write/edit 工具触发
//! - 在文件所在目录有 git 仓库时，创建带特殊前缀的 commit 作为快照点
//! - 快照失败不阻塞工具执行，仅记录日志
//!
//! 快照 commit message 格式：`ccode-snapshot: <tool_name> <file_path>`
//! 用户可通过 `git log --oneline --grep=ccode-snapshot` 查看快照历史，
//! 通过 `git revert` 或 `git checkout` 回退到任意快照点。

use std::future::Future;
use std::pin::Pin;

use super::bridge::PreExecuteHook;

/// Git 快照钩子
/// Write/Edit 工具执行前，自动创建 git 快照点用于 undo
pub struct GitSnapshotHook;

impl GitSnapshotHook {
    /// 从文件路径推断 git 工作目录
    ///
    /// 优先取文件的父目录，回退到当前进程目录。
    fn resolve_workdir(file_path: &str) -> std::path::PathBuf {
        let path = std::path::Path::new(file_path);
        if path.is_absolute() {
            path.parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
        } else {
            std::env::current_dir()
                .unwrap_or_default()
                .join(path.parent().unwrap_or(std::path::Path::new("")))
        }
    }

    /// 检查目录是否在 git 仓库中
    fn is_git_repo(workdir: &std::path::Path) -> bool {
        std::process::Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(workdir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// 创建 git 快照 commit
    ///
    /// 流程：git add <file> → git commit -m "ccode-snapshot: ..."
    /// 仅暂存被修改的文件，不影响其他未跟踪/未修改文件。
    /// 如果没有变更（clean working tree），跳过 commit。
    fn create_snapshot(workdir: &std::path::Path, tool_name: &str, file_path: &str) -> Option<String> {
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain", "--", file_path])
            .current_dir(workdir)
            .output()
            .ok()?;

        let status_str = String::from_utf8_lossy(&status.stdout);
        if status_str.trim().is_empty() {
            return None;
        }

        let add_status = std::process::Command::new("git")
            .args(["add", "--", file_path])
            .current_dir(workdir)
            .status();

        if let Err(e) = add_status {
            tracing::warn!("git add 快照失败：{}", e);
            return None;
        }

        // 检查 git user.name / user.email 是否配置；缺失时用 -c 临时注入，避免 commit 失败
        let has_user_name = std::process::Command::new("git")
            .args(["config", "user.name"])
            .current_dir(workdir)
            .output()
            .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        let has_user_email = std::process::Command::new("git")
            .args(["config", "user.email"])
            .current_dir(workdir)
            .output()
            .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        let message = format!("ccode-snapshot: {} {}", tool_name, file_path);
        let mut commit_cmd = std::process::Command::new("git");
        if !has_user_name {
            commit_cmd.args(["-c", "user.name=ccode"]);
        }
        if !has_user_email {
            commit_cmd.args(["-c", "user.email=ccode@local"]);
        }
        let commit_result = commit_cmd
            .args(["commit", "-m", &message, "--quiet", "--no-verify"])
            .current_dir(workdir)
            .output();

        match commit_result {
            Ok(output) if output.status.success() => {
                let hash = std::process::Command::new("git")
                    .args(["rev-parse", "--short", "HEAD"])
                    .current_dir(workdir)
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();

                Some(format!(
                    "\n[git 快照] 已创建快照点 {}（`git revert {}` 可回退）\n",
                    hash, hash
                ))
            }
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("git commit 快照失败：{}", e);
                None
            }
        }
    }
}

impl PreExecuteHook for GitSnapshotHook {
    fn should_run(&self, tool_name: &str) -> bool {
        tool_name == "write_file" || tool_name == "search_replace"
    }

    fn run<'a>(
        &'a self,
        tool_name: &str,
        args: &'a serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        let filepath = args
            .get("file_path")
            .or_else(|| args.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let tool_name = tool_name.to_string();

        Box::pin(async move {
            if filepath.is_empty() {
                return String::new();
            }

            let workdir = Self::resolve_workdir(&filepath);
            if !Self::is_git_repo(&workdir) {
                return String::new();
            }

            Self::create_snapshot(&workdir, &tool_name, &filepath)
                .unwrap_or_default()
        })
    }
}
