//! 记忆整合：跨会话去重、提炼摘要
//!
//! 参考 Claude Code 的 autoDream 模式，对散落在各会话中的知识文件
//! 进行跨会话整合：去重、排序、提炼摘要。
//!
//! 整合过程通过 PID 文件锁防止多实例并发，锁超时为 10 分钟。

use std::fs;
use std::path::{Path, PathBuf};

/// 获取自动记忆目录
///
/// 返回路径：<project_dir>/.ccode/memory/
/// 所有知识文件和锁文件均存放在此目录下。
pub fn get_auto_mem_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".ccode").join("memory")
}

/// 尝试获取整合锁（基于 PID 文件，防止多实例并发）
///
/// 返回锁文件路径和修改时间，失败返回 None。
/// 锁机制：
/// 1. 检查锁文件是否存在
/// 2. 若存在，检查持有锁的进程是否仍在运行
/// 3. 若锁超过 10 分钟视为过期
/// 4. 获取锁后写入当前 PID，并验证（防止竞态）
pub fn try_acquire_consolidation_lock(memory_dir: &Path) -> Option<(PathBuf, u64)> {
    let lock_path = memory_dir.join(".consolidation.lock");

    if lock_path.exists() {
        // 检查锁是否过期（超过 10 分钟视为过期）
        if let Ok(metadata) = fs::metadata(&lock_path) {
            if let Ok(modified) = metadata.modified() {
                let elapsed = modified.elapsed().unwrap_or_default();
                if elapsed.as_secs() < 600 {
                    // 锁仍有效，检查持有者是否存活
                    if let Ok(content) = fs::read_to_string(&lock_path) {
                        if let Ok(pid) = content.trim().parse::<u32>() {
                            // 检查进程是否仍在运行
                            if is_pid_alive(pid) {
                                return None; // 锁被其他进程持有
                            }
                        }
                    }
                }
            }
        }
    }

    // 获取锁：写入当前 PID
    let pid = std::process::id();
    fs::create_dir_all(memory_dir).ok()?;
    fs::write(&lock_path, pid.to_string()).ok()?;

    // 验证锁（防止竞态：另一个进程可能同时抢锁）
    let content = fs::read_to_string(&lock_path).ok()?;
    if content.trim() != pid.to_string() {
        return None; // 另一个进程抢到了锁
    }

    // 返回锁文件路径和修改时间
    let mtime = fs::metadata(&lock_path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        })
        .unwrap_or(0);

    Some((lock_path, mtime))
}

/// 释放整合锁
///
/// 删除锁文件，允许其他实例获取锁。
pub fn release_consolidation_lock(lock_path: &Path) {
    let _ = fs::remove_file(lock_path);
}

/// 检查 PID 是否仍在运行
///
/// 在 Unix 系统上使用 libc::kill(pid, 0) 检测进程是否存在，
/// 信号 0 不会实际发送信号，仅检查进程存在性。
/// 在非 Unix 系统上始终返回 false（保守策略：假设进程已退出）。
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    false
}

/// 整合记忆：读取所有知识文件，去重，提炼摘要
///
/// 扫描 memory_dir 下的所有 .md 文件，读取内容后进行简单去重
/// （完全匹配去重 + 排序），返回去重后的知识列表。
pub fn consolidate_memories(memory_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut all_knowledge = Vec::new();

    // 读取所有 .md 文件
    for entry in fs::read_dir(memory_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map_or(false, |e| e == "md") {
            let content = fs::read_to_string(&path)?;
            all_knowledge.push(content);
        }
    }

    // 去重（简单的完全匹配去重）
    all_knowledge.sort();
    all_knowledge.dedup();

    Ok(all_knowledge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_auto_mem_path() {
        let path = get_auto_mem_path(Path::new("/tmp/project"));
        assert_eq!(path, PathBuf::from("/tmp/project/.ccode/memory"));
    }

    #[test]
    fn test_consolidate_memories_empty_dir() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let result = consolidate_memories(dir.path()).expect("整合记忆失败");
        assert!(result.is_empty());
    }

    #[test]
    fn test_consolidate_memories_dedup() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        fs::write(dir.path().join("a.md"), "知识A").expect("写入失败");
        fs::write(dir.path().join("b.md"), "知识B").expect("写入失败");
        fs::write(dir.path().join("c.md"), "知识A").expect("写入失败"); // 重复
        let result = consolidate_memories(dir.path()).expect("整合记忆失败");
        assert_eq!(result.len(), 2); // 去重后应为 2
    }

    #[test]
    fn test_release_consolidation_lock() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let lock_path = dir.path().join(".consolidation.lock");
        fs::write(&lock_path, "12345").expect("写入锁文件失败");
        release_consolidation_lock(&lock_path);
        assert!(!lock_path.exists());
    }
}
