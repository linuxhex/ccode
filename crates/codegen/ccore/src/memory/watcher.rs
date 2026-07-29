//! 文件监听器（借鉴 Claude Code MemoryWatcher）
//!
//! 监听 ~/.ccode/memory/ 下的 MEMORY.md 文件变化，
//! 变化时触发 reindex 操作。
//! 当前使用轮询方式实现，生产环境可替换为 notify crate。

use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::mpsc;
use tokio::time;

/// 记忆文件监听器
pub struct MemoryWatcher {
    /// 监听路径
    watch_path: PathBuf,
    /// 事件发送端
    event_tx: mpsc::Sender<MemoryWatchEvent>,
    /// 事件接收端（由消费者获取）
    event_rx: Option<mpsc::Receiver<MemoryWatchEvent>>,
}

/// 监听事件
#[derive(Debug, Clone)]
pub enum MemoryWatchEvent {
    /// 文件创建
    Created { path: PathBuf },
    /// 文件修改
    Modified { path: PathBuf },
    /// 文件删除
    Deleted { path: PathBuf },
}

/// 文件状态追踪
#[derive(Debug, Clone)]
struct FileState {
    /// 最后修改时间
    modified: Option<SystemTime>,
}

impl MemoryWatcher {
    /// 创建新的记忆文件监听器
    pub fn new(watch_path: PathBuf) -> Self {
        let (event_tx, event_rx) = mpsc::channel(64);
        Self {
            watch_path,
            event_tx,
            event_rx: Some(event_rx),
        }
    }

    /// 启动监听（spawn 一个 tokio task）
    ///
    /// 使用轮询方式检查文件变化，每 2 秒检查一次。
    /// 监听 watch_path 目录下的所有 .md 文件。
    pub async fn start(&mut self) -> anyhow::Result<()> {
        let watch_path = self.watch_path.clone();
        let event_tx = self.event_tx.clone();

        // 初始化文件状态
        let mut file_states: std::collections::HashMap<PathBuf, FileState> =
            std::collections::HashMap::new();

        // 扫描初始状态
        scan_directory(&watch_path, &mut file_states);

        // 启动轮询任务
        tokio::spawn(async move {
            let mut interval = time::interval(time::Duration::from_secs(2));

            loop {
                interval.tick().await;

                let old_states = file_states.clone();
                let mut new_states: std::collections::HashMap<PathBuf, FileState> =
                    std::collections::HashMap::new();

                // 扫描当前状态
                scan_directory(&watch_path, &mut new_states);

                // 检测变化
                // 1. 新增或修改文件
                for (path, new_state) in &new_states {
                    if let Some(old_state) = old_states.get(path) {
                        // 已有文件：检查修改
                        if old_state.modified != new_state.modified
                            && new_state.modified.is_some()
                        {
                            let _ = event_tx
                                .send(MemoryWatchEvent::Modified {
                                    path: path.clone(),
                                })
                                .await;
                        }
                    } else {
                        // 新文件
                        let _ = event_tx
                            .send(MemoryWatchEvent::Created {
                                path: path.clone(),
                            })
                            .await;
                    }
                }

                // 2. 删除文件
                for path in old_states.keys() {
                    if !new_states.contains_key(path.as_path()) {
                        let _ = event_tx
                            .send(MemoryWatchEvent::Deleted {
                                path: path.clone(),
                            })
                            .await;
                    }
                }

                file_states = new_states;
            }
        });

        Ok(())
    }

    /// 获取事件接收端
    pub fn take_event_rx(&mut self) -> Option<mpsc::Receiver<MemoryWatchEvent>> {
        self.event_rx.take()
    }
}

/// 扫描目录，递归记录 .md 文件状态
fn scan_directory(
    dir: &PathBuf,
    states: &mut std::collections::HashMap<PathBuf, FileState>,
) {
    if !dir.exists() {
        return;
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_directory(&path, states);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let modified = path.metadata().ok().and_then(|m| m.modified().ok());
                states.insert(
                    path,
                    FileState {
                        modified,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_watcher_new() {
        let tmp = TempDir::new().unwrap();
        let watch_path = tmp.path().to_path_buf();
        let mut watcher = MemoryWatcher::new(watch_path);

        assert!(watcher.take_event_rx().is_some());
        assert!(watcher.take_event_rx().is_none()); // 只能取一次
    }

    #[tokio::test]
    async fn test_watcher_detects_creation() {
        let tmp = TempDir::new().unwrap();
        let watch_path = tmp.path().join("memory");
        fs::create_dir_all(&watch_path).unwrap();

        let mut watcher = MemoryWatcher::new(watch_path.clone());
        let mut rx = watcher.take_event_rx().unwrap();

        watcher.start().await.unwrap();

        // 创建文件
        fs::write(watch_path.join("MEMORY.md"), "# Test").unwrap();

        // 等待轮询检测
        time::sleep(time::Duration::from_millis(2500)).await;

        // 检查是否收到事件
        let mut received_create = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, MemoryWatchEvent::Created { .. }) {
                received_create = true;
            }
        }
        assert!(received_create, "should detect file creation");
    }

    #[tokio::test]
    async fn test_watcher_detects_modification() {
        let tmp = TempDir::new().unwrap();
        let watch_path = tmp.path().join("memory");
        fs::create_dir_all(&watch_path).unwrap();
        let file_path = watch_path.join("MEMORY.md");
        fs::write(&file_path, "# Initial").unwrap();

        let mut watcher = MemoryWatcher::new(watch_path.clone());
        let mut rx = watcher.take_event_rx().unwrap();

        watcher.start().await.unwrap();

        // 修改文件
        time::sleep(time::Duration::from_millis(500)).await;
        fs::write(&file_path, "# Modified").unwrap();

        // 等待轮询检测
        time::sleep(time::Duration::from_millis(2500)).await;

        let mut received_modify = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, MemoryWatchEvent::Modified { .. }) {
                received_modify = true;
            }
        }
        assert!(received_modify, "should detect file modification");
    }

    #[tokio::test]
    async fn test_watcher_detects_deletion() {
        let tmp = TempDir::new().unwrap();
        let watch_path = tmp.path().join("memory");
        fs::create_dir_all(&watch_path).unwrap();
        let file_path = watch_path.join("MEMORY.md");
        fs::write(&file_path, "# Test").unwrap();

        let mut watcher = MemoryWatcher::new(watch_path.clone());
        let mut rx = watcher.take_event_rx().unwrap();

        watcher.start().await.unwrap();

        // 删除文件
        time::sleep(time::Duration::from_millis(500)).await;
        fs::remove_file(&file_path).unwrap();

        // 等待轮询检测
        time::sleep(time::Duration::from_millis(2500)).await;

        let mut received_delete = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, MemoryWatchEvent::Deleted { .. }) {
                received_delete = true;
            }
        }
        assert!(received_delete, "should detect file deletion");
    }
}
