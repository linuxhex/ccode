//! 读取追踪器（借鉴 Claude Code 先读后写约束）
//!
//! 追踪哪些文件已被读取，确保 Write/Edit 前必须先 Read
//! Claude Code 的 FileWriteTool 和 FileEditTool 都要求：
//! "You MUST read a file before editing it"

use std::collections::HashSet;
use std::sync::Mutex;

/// 读取追踪器
///
/// 使用全局静态实例追踪当前会话中已读取的文件。
/// Write/Edit 操作前会检查文件是否已被读取，未读取则拒绝操作。
pub struct ReadTracker {
    /// 已读取的文件集合（规范化后的绝对路径）
    read_files: Mutex<HashSet<String>>,
}

impl ReadTracker {
    /// 创建新的追踪器
    pub fn new() -> Self {
        Self {
            read_files: Mutex::new(HashSet::new()),
        }
    }

    /// 记录文件已被读取
    pub fn mark_read(&self, path: &str) {
        if let Ok(mut files) = self.read_files.lock() {
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::push_held("READ_TRACKER");
                crate::kernel::lock_order::checker::check_order("READ_TRACKER");
            }
            files.insert(path.to_string());
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::pop_held("READ_TRACKER");
            }
        }
    }

    /// 检查文件是否已被读取
    pub fn has_been_read(&self, path: &str) -> bool {
        if let Ok(files) = self.read_files.lock() {
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::push_held("READ_TRACKER");
                crate::kernel::lock_order::checker::check_order("READ_TRACKER");
            }
            let result = files.contains(path);
            #[cfg(debug_assertions)]
            {
                crate::kernel::lock_order::checker::pop_held("READ_TRACKER");
            }
            result
        } else {
            false
        }
    }

    /// 要求文件必须先被读取
    ///
    /// 返回 Err 如果文件未被读取
    pub fn require_read(&self, path: &str) -> anyhow::Result<()> {
        if self.has_been_read(path) {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "必须先读取文件 {} 才能写入或编辑。\n请先使用 read 工具读取此文件。",
                path
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

/// 标记文件已被读取（便捷函数）
pub fn mark_file_read(path: &str) {
    global_read_tracker().mark_read(path);
}

/// 检查文件是否已被读取（便捷函数）
pub fn has_file_been_read(path: &str) -> bool {
    global_read_tracker().has_been_read(path)
}

/// 要求文件必须先被读取（便捷函数）
pub fn require_file_read(path: &str) -> anyhow::Result<()> {
    global_read_tracker().require_read(path)
}

/// 清除所有追踪记录（便捷函数）
pub fn clear_all() {
    global_read_tracker().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mark_and_check() {
        let tracker = ReadTracker::new();
        assert!(!tracker.has_been_read("/src/main.rs"));
        tracker.mark_read("/src/main.rs");
        assert!(tracker.has_been_read("/src/main.rs"));
    }

    #[test]
    fn test_require_read_success() {
        let tracker = ReadTracker::new();
        tracker.mark_read("/src/lib.rs");
        assert!(tracker.require_read("/src/lib.rs").is_ok());
    }

    #[test]
    fn test_require_read_failure() {
        let tracker = ReadTracker::new();
        let result = tracker.require_read("/src/unknown.rs");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("必须先读取"));
    }

    #[test]
    fn test_clear() {
        let tracker = ReadTracker::new();
        tracker.mark_read("/src/a.rs");
        tracker.mark_read("/src/b.rs");
        assert!(tracker.has_been_read("/src/a.rs"));
        tracker.clear();
        assert!(!tracker.has_been_read("/src/a.rs"));
        assert!(!tracker.has_been_read("/src/b.rs"));
    }

    #[test]
    fn test_global_tracker() {
        let tracker = global_read_tracker();
        tracker.mark_read("/test/global.rs");
        assert!(has_file_been_read("/test/global.rs"));
        assert!(require_file_read("/test/global.rs").is_ok());
        assert!(!has_file_been_read("/test/nonexistent.rs"));
    }

    #[test]
    fn test_different_paths_tracked_independently() {
        let tracker = ReadTracker::new();
        tracker.mark_read("/src/a.rs");
        assert!(tracker.has_been_read("/src/a.rs"));
        assert!(!tracker.has_been_read("/src/b.rs"));
    }

    #[test]
    fn test_clear_all_convenience() {
        mark_file_read("/test/clear_all.rs");
        assert!(has_file_been_read("/test/clear_all.rs"));
        clear_all();
        assert!(!has_file_been_read("/test/clear_all.rs"));
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::thread;
    use std::sync::Arc;

    #[test]
    fn test_concurrent_read_tracking() {
        let tracker = Arc::new(ReadTracker::new());
        let mut handles = vec![];
        
        for i in 0..10 {
            let tracker = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                let path = format!("/tmp/test_file_{}.rs", i);
                tracker.mark_read(&path);
                assert!(tracker.has_been_read(&path));
            }));
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        // All files should be tracked
        for i in 0..10 {
            assert!(tracker.has_been_read(&format!("/tmp/test_file_{}.rs", i)));
        }
    }

    #[test]
    fn test_concurrent_read_write_race() {
        let tracker = Arc::new(ReadTracker::new());
        let path = "/tmp/race_test.rs";
        
        let mut handles = vec![];
        for _ in 0..10 {
            let tracker = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                tracker.mark_read(path);
                let _ = tracker.require_read(path);
            }));
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
    }
}
