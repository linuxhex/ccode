//! 三级持久化存储（借鉴 Claude Code MemoryStorage）
//!
//! 存储层级：
//! - Global: ~/.ccode/memory/MEMORY.md
//! - Workspace: ~/.ccode/memory/{workspace_hash}/MEMORY.md
//! - Session: ~/.ccode/memory/{workspace_hash}/sessions/{date}-{slug}-{sid}.md
//!
//! workspace_hash = blake3(cwd)[..16]（与 Claude Code 兼容）

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

/// 记忆存储范围
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryScope {
    Global,
    Workspace,
    Session,
}

/// 记忆存储
pub struct MemoryStorage {
    /// 根目录 ~/.ccode/memory/
    root: PathBuf,
    /// workspace hash (blake3 of cwd, first 16 chars)
    workspace_hash: String,
}

impl MemoryStorage {
    /// 创建新的记忆存储实例
    ///
    /// # Arguments
    /// * `root` - 根目录路径（如 ~/.ccode/memory/）
    /// * `cwd` - 当前工作目录路径
    pub fn new(root: PathBuf, cwd: &str) -> Self {
        let workspace_hash = Self::compute_workspace_hash(cwd);
        Self { root, workspace_hash }
    }

    /// 使用默认根目录创建（~/.ccode/memory/）
    pub fn with_default_root(cwd: &str) -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let root = PathBuf::from(home).join(".ccode").join("memory");
        Self::new(root, cwd)
    }

    /// 确保 MEMORY.md 文件存在
    pub async fn ensure_initialized(&self) -> anyhow::Result<()> {
        // 确保根目录存在
        fs::create_dir_all(&self.root).await?;

        // 确保 Global MEMORY.md 存在
        let global_file = self.root.join("MEMORY.md");
        if !global_file.exists() {
            let template = "# Global Memory\n\
                \n\
                > This file is automatically managed by ccode's memory system.\n\
                > You can also edit it manually — changes will be indexed on next session.\n\
                \n\
                ## Preferences\n\
                \n\
                <!-- Add any cross-project preferences here -->\n";
            fs::write(&global_file, template).await?;
        }

        // 确保 workspace 目录存在
        let ws_dir = self.workspace_dir();
        fs::create_dir_all(&ws_dir).await?;

        // 确保 workspace MEMORY.md 存在
        let ws_file = ws_dir.join("MEMORY.md");
        if !ws_file.exists() {
            let template = "# Project Memory\n\
                \n\
                > Auto-populated by dream consolidation. Edit freely.\n";
            fs::write(&ws_file, template).await?;
        }

        Ok(())
    }

    /// 读取指定范围的 MEMORY.md
    pub async fn read(&self, scope: MemoryScope) -> anyhow::Result<Option<String>> {
        let path = match scope {
            MemoryScope::Global => self.root.join("MEMORY.md"),
            MemoryScope::Workspace => self.workspace_dir().join("MEMORY.md"),
            MemoryScope::Session => return Ok(None), // Session 需要指定具体文件
        };

        if path.exists() {
            let content = fs::read_to_string(&path).await?;
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }

    /// 写入指定范围的 MEMORY.md
    pub async fn write(&self, scope: MemoryScope, content: &str) -> anyhow::Result<()> {
        let path = match scope {
            MemoryScope::Global => {
                fs::create_dir_all(&self.root).await?;
                self.root.join("MEMORY.md")
            }
            MemoryScope::Workspace => {
                let ws_dir = self.workspace_dir();
                fs::create_dir_all(&ws_dir).await?;
                ws_dir.join("MEMORY.md")
            }
            MemoryScope::Session => {
                return Err(anyhow::anyhow!("Session scope requires write_session method"));
            }
        };

        fs::write(&path, content).await?;
        Ok(())
    }

    /// 追加内容到指定范围
    pub async fn append(&self, scope: MemoryScope, content: &str) -> anyhow::Result<()> {
        let path = match scope {
            MemoryScope::Global => {
                fs::create_dir_all(&self.root).await?;
                self.root.join("MEMORY.md")
            }
            MemoryScope::Workspace => {
                let ws_dir = self.workspace_dir();
                fs::create_dir_all(&ws_dir).await?;
                ws_dir.join("MEMORY.md")
            }
            MemoryScope::Session => {
                return Err(anyhow::anyhow!("Session scope requires write_session method"));
            }
        };

        if path.exists() {
            let existing = fs::read_to_string(&path).await?;
            let new_content = if existing.is_empty() {
                content.to_string()
            } else {
                format!("{}\n\n{}", existing.trim_end(), content)
            };
            fs::write(&path, new_content).await?;
        } else {
            fs::write(&path, content).await?;
        }

        Ok(())
    }

    /// 列出 session 文件
    pub async fn list_sessions(&self) -> anyhow::Result<Vec<PathBuf>> {
        let session_dir = self.session_dir();
        if !session_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        let mut dir = fs::read_dir(&session_dir).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                sessions.push(path);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    /// 写入 session 文件
    pub async fn write_session(
        &self,
        date: &str,
        slug: &str,
        sid: &str,
        content: &str,
    ) -> anyhow::Result<PathBuf> {
        let session_dir = self.session_dir();
        fs::create_dir_all(&session_dir).await?;

        let sid_short = &sid[..sid.len().min(8)];
        let filename = format!("{}-{}-{}.md", date, slug, sid_short);
        let path = session_dir.join(&filename);

        fs::write(&path, content).await?;
        Ok(path)
    }

    /// 清除 workspace 记忆
    pub async fn clear_workspace(&self) -> anyhow::Result<bool> {
        let ws_dir = self.workspace_dir();
        if ws_dir.exists() {
            fs::remove_dir_all(&ws_dir).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 计算 workspace hash (blake3)
    fn compute_workspace_hash(cwd: &str) -> String {
        let hash = blake3::hash(cwd.as_bytes());
        hash.to_hex()[..16].to_string()
    }

    /// 获取 workspace 目录
    fn workspace_dir(&self) -> PathBuf {
        self.root.join(&self.workspace_hash)
    }

    /// 获取 session 目录
    fn session_dir(&self) -> PathBuf {
        self.workspace_dir().join("sessions")
    }

    /// 获取根目录
    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    /// 获取 workspace hash
    pub fn workspace_hash(&self) -> &str {
        &self.workspace_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_compute_workspace_hash_deterministic() {
        let hash1 = MemoryStorage::compute_workspace_hash("/some/workspace");
        let hash2 = MemoryStorage::compute_workspace_hash("/some/workspace");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_workspace_hash_different_paths() {
        let hash1 = MemoryStorage::compute_workspace_hash("/workspace/a");
        let hash2 = MemoryStorage::compute_workspace_hash("/workspace/b");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_workspace_hash_length() {
        let hash = MemoryStorage::compute_workspace_hash("/test/path");
        assert_eq!(hash.len(), 16, "hash should be 16 hex chars");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn test_ensure_initialized() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage.ensure_initialized().await.unwrap();

        assert!(root.join("MEMORY.md").exists());
        assert!(root.join(storage.workspace_hash()).join("MEMORY.md").exists());

        // 幂等：再次调用不应覆盖
        let content_before = std::fs::read_to_string(root.join("MEMORY.md")).unwrap();
        storage.ensure_initialized().await.unwrap();
        let content_after = std::fs::read_to_string(root.join("MEMORY.md")).unwrap();
        assert_eq!(content_before, content_after);
    }

    #[tokio::test]
    async fn test_read_write_global() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage
            .write(MemoryScope::Global, "# Global\n\nContent")
            .await
            .unwrap();

        let content = storage.read(MemoryScope::Global).await.unwrap();
        assert_eq!(content, Some("# Global\n\nContent".to_string()));
    }

    #[tokio::test]
    async fn test_read_write_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage
            .write(MemoryScope::Workspace, "# Project\n\nInfo")
            .await
            .unwrap();

        let content = storage.read(MemoryScope::Workspace).await.unwrap();
        assert_eq!(content, Some("# Project\n\nInfo".to_string()));
    }

    #[tokio::test]
    async fn test_append() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage
            .write(MemoryScope::Global, "# First")
            .await
            .unwrap();
        storage
            .append(MemoryScope::Global, "## Second")
            .await
            .unwrap();

        let content = storage.read(MemoryScope::Global).await.unwrap();
        let content = content.unwrap();
        assert!(content.contains("# First"));
        assert!(content.contains("## Second"));
    }

    #[tokio::test]
    async fn test_session_write_and_list() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage.ensure_initialized().await.unwrap();

        let path = storage
            .write_session("2026-07-29", "fix-auth", "abc123456789", "Session log")
            .await
            .unwrap();

        assert!(path.exists());
        assert!(path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("2026-07-29-fix-auth-abc12345"));

        let sessions = storage.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[tokio::test]
    async fn test_clear_workspace() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("memory");
        let storage = MemoryStorage::new(root.clone(), "/test/project");

        storage.ensure_initialized().await.unwrap();
        let ws_dir = root.join(storage.workspace_hash());
        assert!(ws_dir.exists());

        let removed = storage.clear_workspace().await.unwrap();
        assert!(removed);
        assert!(!ws_dir.exists());

        // 再次清除应返回 false
        let removed = storage.clear_workspace().await.unwrap();
        assert!(!removed);
    }
}
