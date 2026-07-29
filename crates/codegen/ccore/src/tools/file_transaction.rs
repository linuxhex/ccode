//! 文件事务管理器 - 多文件原子写入与失败回滚
//!
//! 在 Agent 修改多个文件前先备份原始内容，若中途失败则逐文件恢复到事务开始前的状态，
//! 避免产生半写半旧的中间态。回滚策略：
//! - 备份有内容：用原始内容覆盖
//! - 备份为 None（文件原本不存在）：删除新增文件

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

// ─── 临时文件守卫 ────────────────────────────────────────────────────────────

/// 临时文件 RAII 守卫
///
/// 创建时记录临时文件路径，如果未被标记为已提交（`committed`），
/// 则在 Drop 时自动清理临时文件，防止因 panic 或提前返回导致临时文件泄漏。
///
/// 典型用法：
/// ```ignore
/// let guard = TempFileGuard::new(&tmp_path);
/// tokio::fs::write(&tmp_path, content).await?;
/// tokio::fs::rename(&tmp_path, &final_path).await?;
/// guard.commit(); // 成功后标记已提交，不再清理
/// ```
pub struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    /// 创建新的临时文件守卫
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            committed: false,
        }
    }

    /// 标记临时文件已成功提交（rename 成功），不再需要清理
    pub fn commit(mut self) {
        self.committed = true;
        // self 会被 drop，committed=true 时不会删除文件
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed && self.path.exists() {
            tracing::debug!(
                target: "ccore::tool",
                temp = %self.path.display(),
                "cleaning up temp file"
            );
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// 文件事务管理器
///
/// 在修改多个文件前备份原始内容，失败时逐文件回滚。
/// 一次事务内对同一文件多次 backup 只记录首次状态，确保回滚到事务开始前的版本。
pub struct FileTransaction {
    /// 备份的文件内容（None 表示文件原本不存在）
    backups: HashMap<PathBuf, Option<Vec<u8>>>,
    /// 事务是否激活
    active: bool,
}

impl FileTransaction {
    pub fn new() -> Self {
        Self {
            backups: HashMap::new(),
            active: false,
        }
    }

    /// 开启事务，清空旧备份并标记激活
    pub fn begin(&mut self) {
        self.backups.clear();
        self.active = true;
    }

    /// 备份文件原始内容
    ///
    /// 若该路径已备份则跳过，保证回滚时恢复的是事务开始前的状态；
    /// 文件不存在则记为 None，回滚时据此删除新建文件。
    pub fn backup(&mut self, path: &Path) -> Result<()> {
        if !self.active {
            anyhow::bail!("事务未激活，请先调用 begin()");
        }
        if self.backups.contains_key(path) {
            return Ok(());
        }
        match std::fs::read(path) {
            Ok(content) => {
                self.backups.insert(path.to_path_buf(), Some(content));
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.backups.insert(path.to_path_buf(), None);
            }
            Err(err) => {
                return Err(anyhow::Error::from(err)
                    .context(format!("备份文件失败：{}", path.display())));
            }
        }
        Ok(())
    }

    /// 提交事务，丢弃备份且不再支持回滚
    pub fn commit(&mut self) {
        self.backups.clear();
        self.active = false;
    }

    /// 回滚事务，逐文件恢复到事务开始前的状态
    ///
    /// 有内容则写回原始字节，无内容（原本不存在）则删除文件。
    /// 单个文件回滚失败不影响其他文件，仅记录 tracing::warn。
    pub fn rollback(&mut self) -> Result<()> {
        if !self.active {
            anyhow::bail!("事务未激活，无法回滚");
        }

        for (path, backup) in &self.backups {
            let restore_result = match backup {
                Some(content) => std::fs::write(path, content),
                None => match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(err) => Err(err),
                },
            };
            if let Err(err) = restore_result {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "回滚文件失败，继续回滚其余文件"
                );
            }
        }

        self.backups.clear();
        self.active = false;
        Ok(())
    }

    /// 事务是否激活
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl Default for FileTransaction {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FileTransaction;
    use std::fs;
    use std::path::PathBuf;

    /// 辅助：构造临时文件路径
    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ccore_file_txn_test_{}", name))
    }

    #[test]
    fn rollback_restores_modified_file() {
        let path = tmp_path("restore_modified");
        fs::write(&path, b"original").expect("写入初始内容失败");

        let mut txn = FileTransaction::new();
        txn.begin();
        txn.backup(&path).expect("备份失败");
        fs::write(&path, b"modified").expect("写入修改内容失败");

        txn.rollback().expect("回滚失败");

        let restored = fs::read_to_string(&path).expect("读取回滚内容失败");
        assert_eq!(restored, "original");
        assert!(!txn.is_active());

        if let Err(e) = fs::remove_file(&path) {
            tracing::debug!("清理临时文件失败：{}", e);
        }
    }

    #[test]
    fn rollback_removes_newly_created_file() {
        let path = tmp_path("remove_new");
        if let Err(e) = fs::remove_file(&path) {
            tracing::debug!("清理临时文件失败：{}", e);
        }

        let mut txn = FileTransaction::new();
        txn.begin();
        txn.backup(&path).expect("备份不存在文件失败");
        fs::write(&path, b"new").expect("写入新文件失败");

        txn.rollback().expect("回滚失败");

        assert!(!path.exists(), "回滚后新建文件应被删除");
    }

    #[test]
    fn backup_is_idempotent() {
        let path = tmp_path("idempotent");
        fs::write(&path, b"v1").expect("写入失败");

        let mut txn = FileTransaction::new();
        txn.begin();
        txn.backup(&path).expect("首次备份失败");
        fs::write(&path, b"v2").expect("写入失败");
        txn.backup(&path).expect("二次备份失败");

        txn.rollback().expect("回滚失败");
        assert_eq!(fs::read_to_string(&path).expect("读取失败"), "v1");

        if let Err(e) = fs::remove_file(&path) {
            tracing::debug!("清理临时文件失败：{}", e);
        }
    }

    #[test]
    fn commit_clears_backups() {
        let path = tmp_path("commit_clears");
        fs::write(&path, b"original").expect("写入失败");

        let mut txn = FileTransaction::new();
        txn.begin();
        txn.backup(&path).expect("备份失败");
        txn.commit();
        assert!(!txn.is_active());

        if let Err(e) = fs::remove_file(&path) {
            tracing::debug!("清理临时文件失败：{}", e);
        }
    }
}
