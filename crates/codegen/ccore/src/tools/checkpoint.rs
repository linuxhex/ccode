//! Git Checkpoint - 每次文件编辑前自动 checkpoint，支持回滚

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Checkpoint 记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub agent_id: String,
    pub timestamp: String,
    pub description: String,
    pub commit_hash: Option<String>,
}

/// Checkpoint 类型：使用 stash 还是 commit 保存
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum CheckpointMode {
    /// 使用 git stash 保存（不产生新的 commit）
    Stash,
    /// 使用 git commit 保存（产生检查点 commit）
    Commit,
}

/// Checkpoint 管理器
pub struct CheckpointManager {
    checkpoints: Vec<Checkpoint>,
    working_dir: String,
    /// 默认 checkpoint 模式
    mode: CheckpointMode,
}

impl CheckpointManager {
    pub fn new(working_dir: String) -> Self {
        Self {
            checkpoints: Vec::new(),
            working_dir,
            mode: CheckpointMode::Stash,
        }
    }

    /// 指定 checkpoint 模式创建
    pub fn with_mode(working_dir: String, mode: CheckpointMode) -> Self {
        Self {
            checkpoints: Vec::new(),
            working_dir,
            mode,
        }
    }

    /// 创建 checkpoint（编辑前调用）
    ///
    /// 根据模式执行 git stash 或 git commit 保存当前工作区状态，
    /// 并记录 commit_hash/stash 引用用于后续回滚
    pub async fn create(&mut self, agent_id: &str, description: &str) -> anyhow::Result<String> {
        let id = uuid::Uuid::new_v4().to_string();
        let timestamp = chrono::Utc::now().to_rfc3339();

        let commit_hash = match self.mode {
            CheckpointMode::Stash => self.git_stash_save(&id, description)?,
            CheckpointMode::Commit => self.git_checkpoint_commit(&id, description)?,
        };

        let checkpoint = Checkpoint {
            id: id.clone(),
            agent_id: agent_id.to_string(),
            timestamp,
            description: description.to_string(),
            commit_hash: Some(commit_hash),
        };
        self.checkpoints.push(checkpoint);
        Ok(id)
    }

    /// 回滚到指定 checkpoint
    ///
    /// 根据模式恢复工作区：
    /// - Stash 模式：git stash pop 恢复对应 stash
    /// - Commit 模式：git checkout 到对应 commit，再恢复工作区
    pub async fn rollback(&mut self, checkpoint_id: &str) -> anyhow::Result<()> {
        let checkpoint = self.checkpoints.iter().find(|c| c.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("检查点不存在：{}", checkpoint_id))?;
        let commit_hash = checkpoint.commit_hash.as_ref()
            .ok_or_else(|| anyhow::anyhow!("检查点 {} 没有 commit hash", checkpoint_id))?;

        match self.mode {
            CheckpointMode::Stash => {
                // 恢复 stash：git stash pop <stash_ref>
                let output = Command::new("git")
                    .args(["stash", "pop", commit_hash])
                    .current_dir(&self.working_dir)
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git stash pop 失败：{}", stderr);
                }
            }
            CheckpointMode::Commit => {
                // 先保存当前未提交的修改到 stash
                let output = Command::new("git")
                    .args(["stash", "save", "--include-untracked", "ccode:rollback-auto-stash"])
                    .current_dir(&self.working_dir)
                    .output();
                if let Err(e) = output {
                    tracing::debug!("git stash save 失败：{}", e);
                }

                // 恢复到检查点 commit：git checkout <hash>
                let output = Command::new("git")
                    .args(["checkout", commit_hash])
                    .current_dir(&self.working_dir)
                    .output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("git checkout {} 失败：{}", commit_hash, stderr);
                }
            }
        }

        Ok(())
    }

    /// 列出所有检查点
    pub fn list(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// 获取最近的 checkpoint
    pub fn latest(&self) -> Option<&Checkpoint> {
        self.checkpoints.last()
    }

    /// 使用 git stash save 创建检查点
    fn git_stash_save(&self, id: &str, description: &str) -> anyhow::Result<String> {
        // 检查是否有未保存的修改
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.working_dir)
            .output()?;
        if status_output.stdout.is_empty() {
            // 工作区干净，无需 stash，获取当前 HEAD
            let head_output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.working_dir)
                .output()?;
            return Ok(String::from_utf8_lossy(&head_output.stdout).trim().to_string());
        }

        // 执行 git stash save，包含未跟踪文件
        let stash_msg = format!("ccode:checkpoint:{}:{}", id, description);
        let output = Command::new("git")
            .args(["stash", "save", "--include-untracked", &stash_msg])
            .current_dir(&self.working_dir)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git stash save 失败：{}", stderr);
        }

        // 获取刚创建的 stash 引用：stash@{0}
        let ref_output = Command::new("git")
            .args(["stash", "list"])
            .current_dir(&self.working_dir)
            .output()?;
        let stash_list = String::from_utf8_lossy(&ref_output.stdout);
        // 解析第一行获取 stash@{0} 的完整引用
        if let Some(first_line) = stash_list.lines().next() {
            // stash 列表格式：stash@{0}: On branch: message
            if let Some(colon_pos) = first_line.find(':') {
                return Ok(first_line[..colon_pos].trim().to_string());
            }
        }

        // 回退：使用 HEAD
        let head_output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.working_dir)
            .output()?;
        Ok(String::from_utf8_lossy(&head_output.stdout).trim().to_string())
    }

    /// 使用 git commit 创建检查点
    fn git_checkpoint_commit(&self, id: &str, description: &str) -> anyhow::Result<String> {
        // 检查是否有未保存的修改
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.working_dir)
            .output()?;
        if status_output.stdout.is_empty() {
            // 工作区干净，返回当前 HEAD
            let head_output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.working_dir)
                .output()?;
            return Ok(String::from_utf8_lossy(&head_output.stdout).trim().to_string());
        }

        // 暂存所有修改（包括未跟踪文件）
        let add_output = Command::new("git")
            .args(["add", "--all"])
            .current_dir(&self.working_dir)
            .output()?;
        if !add_output.status.success() {
            let stderr = String::from_utf8_lossy(&add_output.stderr);
            anyhow::bail!("git add --all 失败：{}", stderr);
        }

        // 创建检查点 commit
        let commit_msg = format!("ccode:checkpoint:{}:{}", id, description);
        let commit_output = Command::new("git")
            .args(["commit", "-m", &commit_msg, "--no-verify"])
            .current_dir(&self.working_dir)
            .output()?;
        if !commit_output.status.success() {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            // "nothing to commit" 不算错误
            if !stderr.contains("nothing to commit") {
                anyhow::bail!("git commit 失败：{}", stderr);
            }
        }

        // 获取新 commit 的 hash
        let head_output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&self.working_dir)
            .output()?;
        Ok(String::from_utf8_lossy(&head_output.stdout).trim().to_string())
    }
}
