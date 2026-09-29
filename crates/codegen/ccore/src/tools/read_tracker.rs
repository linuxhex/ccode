//! 读取追踪器（借鉴 Claude Code 先读后写约束）
//!
//! 追踪哪些文件已被读取，确保 Write/Edit 前必须先 Read。
//! 同时记录文件的 mtime 和 len，检测外部修改（版本追踪）。
//!
//! Claude Code 的 FileWriteTool 和 FileEditTool 都要求：
//! "You MUST read a file before editing it"

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// 文件戳记：记录读取时的文件状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStamp {
    /// 最后修改时间
    pub mtime: Option<SystemTime>,
    /// 文件大小（字节）
    pub len: u64,
}

impl FileStamp {
    /// 从文件元数据创建戳记
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            mtime: metadata.modified().ok(),
            len: metadata.len(),
        }
    }

    /// 检查当前文件是否与戳记匹配（未发生外部修改）
    pub fn matches_current(&self, path: &Path) -> bool {
        match std::fs::metadata(path) {
            Ok(metadata) => {
                let current = FileStamp::from_metadata(&metadata);
                // 如果 mtime 和 len 都匹配，认为文件未变化
                // 注意：某些文件系统 mtime 精度有限，所以同时检查 len
                self.len == current.len && self.mtime == current.mtime
            }
            Err(_) => false, // 文件不存在或无法访问，认为已变化
        }
    }
}

/// 读取追踪器
///
/// 使用全局静态实例追踪当前会话中已读取的文件及其版本信息。
/// Write/Edit 操作前会检查文件是否已被读取且未发生外部修改。
pub struct ReadTracker {
    /// 已读取的文件及其戳记（规范化后的绝对路径）
    read_files: Mutex<HashMap<PathBuf, FileStamp>>,
}

impl ReadTracker {
    /// 创建新的追踪器
    pub fn new() -> Self {
        Self {
            read_files: Mutex::new(HashMap::new()),
        }
    }

    /// 记录文件已被读取（带版本戳记）
    pub fn mark_read(&self, path: &Path, stamp: FileStamp) {
        if let Ok(mut files) = self.read_files.lock() {
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::push_held("READ_TRACKER");
                crate::kernel::lock_order::checker::check_order("READ_TRACKER");
            }
            files.insert(path.to_path_buf(), stamp);
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::pop_held("READ_TRACKER");
            }
        }
    }

    /// 记录文件已被读取（简单版本，不记录戳记）
    ///
    /// 用于向后兼容，创建一个占位戳记
    pub fn mark_read_simple(&self, path: &Path) {
        self.mark_read(
            path,
            FileStamp {
                mtime: None,
                len: 0,
            },
        );
    }

    /// 检查文件是否已被读取
    pub fn has_been_read(&self, path: &Path) -> bool {
        if let Ok(files) = self.read_files.lock() {
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::push_held("READ_TRACKER");
                crate::kernel::lock_order::checker::check_order("READ_TRACKER");
            }
            let result = files.contains_key(path);
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::pop_held("READ_TRACKER");
            }
            result
        } else {
            false
        }
    }

    /// 获取文件的读取戳记
    pub fn get_stamp(&self, path: &Path) -> Option<FileStamp> {
        if let Ok(files) = self.read_files.lock() {
            files.get(path).cloned()
        } else {
            None
        }
    }

    /// 检查文件是否已被读取且未发生外部修改
    ///
    /// 返回 Ok(()) 如果：
    /// - 文件已被读取，且
    /// - 文件的 mtime/len 与读取时一致（未发生外部修改）
    ///
    /// 返回 Err 如果：
    /// - 文件未被读取，或
    /// - 文件已被读取但发生了外部修改
    pub fn require_read(&self, path: &Path) -> anyhow::Result<()> {
        let stamp = self.get_stamp(path).ok_or_else(|| {
            anyhow::anyhow!(
                "必须先读取文件 {} 才能写入或编辑。\n请先使用 read 工具读取此文件。",
                path.display()
            )
        })?;

        // 如果有戳记信息，检查文件是否被外部修改
        if (stamp.mtime.is_some() || stamp.len > 0)
            && !stamp.matches_current(path) {
                return Err(anyhow::anyhow!(
                    "文件 {} 在读取后被外部修改。\n请重新读取文件以获取最新内容。",
                    path.display()
                ));
            }

        Ok(())
    }

    /// 要求文件必须先被读取（宽松版本，不检查版本）
    ///
    /// 只检查文件是否被读取过，不检查是否被外部修改。
    /// 用于 search_replace 等有部分保护的工具。
    pub fn require_read_lenient(&self, path: &Path) -> anyhow::Result<()> {
        if self.has_been_read(path) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "必须先读取文件 {} 才能写入或编辑。\n请先使用 read 工具读取此文件。",
                path.display()
            ))
        }
    }

    /// 清除追踪记录
    pub fn clear(&self) {
        if let Ok(mut files) = self.read_files.lock() {
            files.clear();
        }
    }
}

impl Default for ReadTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局读取追踪器
///
/// 使用 std::sync::OnceLock 实现懒初始化，无需外部依赖
static READ_TRACKER: std::sync::OnceLock<ReadTracker> = std::sync::OnceLock::new();

/// 获取全局读取追踪器实例
pub fn global_read_tracker() -> &'static ReadTracker {
    READ_TRACKER.get_or_init(ReadTracker::new)
}

/// 标记文件已被读取（便捷函数，带版本戳记）
pub fn mark_file_read_with_stamp(path: &Path, stamp: FileStamp) {
    global_read_tracker().mark_read(path, stamp);
}

/// 标记文件已被读取（便捷函数，简单版本）
pub fn mark_file_read(path: &Path) {
    global_read_tracker().mark_read_simple(path);
}

/// 检查文件是否已被读取（便捷函数）
pub fn has_file_been_read(path: &Path) -> bool {
    global_read_tracker().has_been_read(path)
}

/// 要求文件必须先被读取且未发生外部修改（严格版本）
pub fn require_file_read(path: &Path) -> anyhow::Result<()> {
    global_read_tracker().require_read(path)
}

/// 要求文件必须先被读取（宽松版本，不检查外部修改）
pub fn require_file_read_lenient(path: &Path) -> anyhow::Result<()> {
    global_read_tracker().require_read_lenient(path)
}

/// 清除所有追踪记录（便捷函数）
pub fn clear_all() {
    global_read_tracker().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_mark_and_check() {
        let tracker = ReadTracker::new();
        let path = Path::new("/src/main.rs");
        assert!(!tracker.has_been_read(path));
        tracker.mark_read_simple(path);
        assert!(tracker.has_been_read(path));
    }

    #[test]
    fn test_require_read_success() {
        let tracker = ReadTracker::new();
        let path = Path::new("/src/lib.rs");
        tracker.mark_read_simple(path);
        assert!(tracker.require_read_lenient(path).is_ok());
    }

    #[test]
    fn test_require_read_failure() {
        let tracker = ReadTracker::new();
        let path = Path::new("/src/unknown.rs");
        let result = tracker.require_read_lenient(path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("必须先读取"));
    }

    #[test]
    fn test_clear() {
        let tracker = ReadTracker::new();
        let a = Path::new("/src/a.rs");
        let b = Path::new("/src/b.rs");
        tracker.mark_read_simple(a);
        tracker.mark_read_simple(b);
        assert!(tracker.has_been_read(a));
        tracker.clear();
        assert!(!tracker.has_been_read(a));
        assert!(!tracker.has_been_read(b));
    }

    #[test]
    fn test_version_tracking_detects_external_change() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // 创建文件并记录初始戳记
        fs::write(&file_path, "initial content").unwrap();
        let metadata = fs::metadata(&file_path).unwrap();
        let initial_stamp = FileStamp::from_metadata(&metadata);

        let tracker = ReadTracker::new();
        tracker.mark_read(&file_path, initial_stamp.clone());

        // 文件未变化时，require_read 应该成功
        assert!(tracker.require_read(&file_path).is_ok());

        // 外部修改文件
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&file_path, "modified content that is different").unwrap();

        // require_read 应该失败（检测到外部修改）
        let result = tracker.require_read(&file_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("外部修改"));
    }

    #[test]
    fn test_lenient_mode_ignores_external_change() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        fs::write(&file_path, "initial content").unwrap();
        let metadata = fs::metadata(&file_path).unwrap();
        let initial_stamp = FileStamp::from_metadata(&metadata);

        let tracker = ReadTracker::new();
        tracker.mark_read(&file_path, initial_stamp);

        // 外部修改文件
        fs::write(&file_path, "modified content").unwrap();

        // 宽松模式应该仍然成功
        assert!(tracker.require_read_lenient(&file_path).is_ok());
        // 严格模式应该失败
        assert!(tracker.require_read(&file_path).is_err());
    }

    #[test]
    fn test_stamp_matches_current() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        fs::write(&file_path, "content").unwrap();
        let metadata = fs::metadata(&file_path).unwrap();
        let stamp = FileStamp::from_metadata(&metadata);

        // 文件未变化
        assert!(stamp.matches_current(&file_path));

        // 修改文件内容（长度变化）
        fs::write(&file_path, "different content here").unwrap();
        assert!(!stamp.matches_current(&file_path));
    }

    #[test]
    fn test_global_tracker() {
        let tracker = global_read_tracker();
        let path = Path::new("/test/global.rs");
        tracker.mark_read_simple(path);
        assert!(has_file_been_read(path));
        assert!(require_file_read_lenient(path).is_ok());
        assert!(!has_file_been_read(Path::new("/test/nonexistent.rs")));
    }

    #[test]
    fn test_different_paths_tracked_independently() {
        let tracker = ReadTracker::new();
        let a = Path::new("/src/a.rs");
        let b = Path::new("/src/b.rs");
        tracker.mark_read_simple(a);
        assert!(tracker.has_been_read(a));
        assert!(!tracker.has_been_read(b));
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_concurrent_read_tracking() {
        let tracker = Arc::new(ReadTracker::new());
        let mut handles = vec![];

        for i in 0..10 {
            let tracker = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                let path = PathBuf::from(format!("/tmp/test_file_{}.rs", i));
                tracker.mark_read_simple(&path);
                assert!(tracker.has_been_read(&path));
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        for i in 0..10 {
            assert!(tracker.has_been_read(&PathBuf::from(format!("/tmp/test_file_{}.rs", i))));
        }
    }

    #[test]
    fn test_concurrent_read_write_race() {
        let tracker = Arc::new(ReadTracker::new());
        let path = PathBuf::from("/tmp/race_test.rs");

        let mut handles = vec![];
        for _ in 0..10 {
            let tracker = Arc::clone(&tracker);
            let path = path.clone();
            handles.push(thread::spawn(move || {
                tracker.mark_read_simple(&path);
                let _ = tracker.require_read_lenient(&path);
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }
}
