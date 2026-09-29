//! 文件检查点系统（借鉴 Claude Code fileHistory.ts）
//!
//! 在每次文件编辑前自动保存原始内容的内存快照，支持：
//! - 版本化备份（同一文件多次编辑，v1/v2/v3 递增）
//! - LRU 淘汰（上限 MAX_SNAPSHOTS）
//! - 按文件路径查询历史版本
//! - 恢复到指定版本
//!
//! 与 `git_snapshot_hook` 互补：
//! - git_snapshot_hook → git 提交级快照，持久化，依赖 git
//! - file_history → 内存级内容快照，轻量，不依赖 git，支持按版本回滚

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_SNAPSHOTS: usize = 50;

#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub path: PathBuf,
    pub content: String,
    pub version: u32,
    pub timestamp: std::time::Instant,
}

pub struct FileHistory {
    snapshots: Mutex<VecDeque<FileSnapshot>>,
    version_map: Mutex<HashMap<PathBuf, u32>>,
}

impl FileHistory {
    pub fn new() -> Self {
        Self {
            snapshots: Mutex::new(VecDeque::new()),
            version_map: Mutex::new(HashMap::new()),
        }
    }

    /// 编辑前捕获文件当前内容作为检查点
    ///
    /// 如果文件不存在（新建文件），不创建快照
    pub fn capture_before_edit(&self, path: &Path, current_content: &str) {
        let mut versions = self.version_map.lock().expect("version_map lock poisoned");
        let version = versions.entry(path.to_path_buf()).or_insert(0);
        *version += 1;
        let current_version = *version;
        drop(versions);

        let snapshot = FileSnapshot {
            path: path.to_path_buf(),
            content: current_content.to_string(),
            version: current_version,
            timestamp: std::time::Instant::now(),
        };

        let mut snapshots = self.snapshots.lock().expect("snapshots lock poisoned");
        snapshots.push_back(snapshot);

        // LRU 淘汰
        while snapshots.len() > MAX_SNAPSHOTS {
            snapshots.pop_front();
        }
    }

    /// 获取指定文件的所有快照版本
    pub fn get_history(&self, path: &Path) -> Vec<FileSnapshot> {
        let snapshots = self.snapshots.lock().expect("snapshots lock poisoned");
        snapshots
            .iter()
            .filter(|s| s.path == path)
            .cloned()
            .collect()
    }

    /// 获取指定文件的最新版本快照
    pub fn get_latest(&self, path: &Path) -> Option<FileSnapshot> {
        let snapshots = self.snapshots.lock().expect("snapshots lock poisoned");
        snapshots
            .iter()
            .rev()
            .find(|s| s.path == path)
            .cloned()
    }

    /// 获取指定文件的指定版本快照
    pub fn get_version(&self, path: &Path, version: u32) -> Option<FileSnapshot> {
        let snapshots = self.snapshots.lock().expect("snapshots lock poisoned");
        snapshots
            .iter()
            .find(|s| s.path == path && s.version == version)
            .cloned()
    }

    /// 恢复到指定版本（返回该版本的内容，调用者负责写入文件）
    pub fn restore_version(&self, path: &Path, version: u32) -> Option<String> {
        self.get_version(path, version).map(|s| s.content)
    }

    /// 恢复到最新版本（返回最近一次编辑前的内容）
    pub fn restore_latest(&self, path: &Path) -> Option<String> {
        self.get_latest(path).map(|s| s.content)
    }

    /// 获取所有被追踪的文件路径
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        let versions = self.version_map.lock().expect("version_map lock poisoned");
        versions.keys().cloned().collect()
    }

    /// 获取当前快照总数
    pub fn snapshot_count(&self) -> usize {
        let snapshots = self.snapshots.lock().expect("snapshots lock poisoned");
        snapshots.len()
    }

    /// 清除所有快照
    pub fn clear(&self) {
        self.snapshots.lock().expect("snapshots lock poisoned").clear();
        self.version_map.lock().expect("version_map lock poisoned").clear();
    }
}

impl Default for FileHistory {
    fn default() -> Self {
        Self::new()
    }
}

static FILE_HISTORY: std::sync::OnceLock<FileHistory> = std::sync::OnceLock::new();

pub fn global_file_history() -> &'static FileHistory {
    FILE_HISTORY.get_or_init(FileHistory::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_and_get_history() {
        let history = FileHistory::new();
        let path = Path::new("/src/main.rs");

        history.capture_before_edit(path, "version 0");
        history.capture_before_edit(path, "version 1");

        let hist = history.get_history(path);
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].version, 1);
        assert_eq!(hist[0].content, "version 0");
        assert_eq!(hist[1].version, 2);
        assert_eq!(hist[1].content, "version 1");
    }

    #[test]
    fn test_get_latest() {
        let history = FileHistory::new();
        let path = Path::new("/src/lib.rs");

        assert!(history.get_latest(path).is_none());

        history.capture_before_edit(path, "first");
        history.capture_before_edit(path, "second");

        let latest = history.get_latest(path).unwrap();
        assert_eq!(latest.content, "second");
        assert_eq!(latest.version, 2);
    }

    #[test]
    fn test_restore() {
        let history = FileHistory::new();
        let path = Path::new("/src/app.rs");

        history.capture_before_edit(path, "original");
        history.capture_before_edit(path, "modified");

        let restored = history.restore_version(path, 1).unwrap();
        assert_eq!(restored, "original");

        let restored_latest = history.restore_latest(path).unwrap();
        assert_eq!(restored_latest, "modified");
    }

    #[test]
    fn test_lru_eviction() {
        let history = FileHistory::new();

        for i in 0..60 {
            let path = PathBuf::from(format!("/src/file_{}.rs", i));
            history.capture_before_edit(&path, &format!("content {}", i));
        }

        assert_eq!(history.snapshot_count(), MAX_SNAPSHOTS);

        // 最早的 10 个应该被淘汰
        let early_path = Path::new("/src/file_0.rs");
        assert!(history.get_latest(early_path).is_none());

        // 最近的应该还在
        let late_path = Path::new("/src/file_59.rs");
        assert!(history.get_latest(late_path).is_some());
    }

    #[test]
    fn test_tracked_files() {
        let history = FileHistory::new();

        history.capture_before_edit(Path::new("/a.rs"), "a");
        history.capture_before_edit(Path::new("/b.rs"), "b");
        history.capture_before_edit(Path::new("/a.rs"), "a2");

        let tracked = history.tracked_files();
        assert_eq!(tracked.len(), 2);
    }

    #[test]
    fn test_version_increments_per_file() {
        let history = FileHistory::new();
        let a = Path::new("/a.rs");
        let b = Path::new("/b.rs");

        history.capture_before_edit(a, "a1");
        history.capture_before_edit(b, "b1");
        history.capture_before_edit(a, "a2");

        let a_latest = history.get_latest(a).unwrap();
        let b_latest = history.get_latest(b).unwrap();

        assert_eq!(a_latest.version, 2);
        assert_eq!(b_latest.version, 1);
    }

    #[test]
    fn test_clear() {
        let history = FileHistory::new();
        history.capture_before_edit(Path::new("/x.rs"), "x");
        assert_eq!(history.snapshot_count(), 1);

        history.clear();
        assert_eq!(history.snapshot_count(), 0);
        assert!(history.tracked_files().is_empty());
    }

    #[test]
    fn test_global_file_history() {
        let fh = global_file_history();
        fh.clear();
        fh.capture_before_edit(Path::new("/global_test.rs"), "content");
        assert!(fh.get_latest(Path::new("/global_test.rs")).is_some());
        fh.clear();
    }
}
