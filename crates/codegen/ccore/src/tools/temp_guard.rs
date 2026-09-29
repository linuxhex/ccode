//! 临时文件 RAII 守卫
//!
//! 创建时记录临时文件路径，如果未被标记为已提交（`committed`），
//! 则在 Drop 时自动清理临时文件，防止因 panic 或提前返回导致临时文件泄漏。
//!
//! 典型用法：
//! ```ignore
//! let guard = TempFileGuard::new(&tmp_path);
//! tokio::fs::write(&tmp_path, content).await?;
//! tokio::fs::rename(&tmp_path, &final_path).await?;
//! guard.commit(); // 成功后标记已提交，不再清理
//! ```

use std::path::{Path, PathBuf};

/// 临时文件 RAII 守卫
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
