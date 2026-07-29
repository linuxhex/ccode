//! 配置文件变更监听
//!
//! 使用 notify crate 监听配置文件变更，发送 ConfigChangeEvent。

use anyhow::Result;
use notify::{Watcher, RecursiveMode, Event};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// 配置变更事件
#[derive(Debug, Clone)]
pub enum ConfigChangeEvent {
    /// Provider 配置变更
    ProviderChanged(PathBuf),
    /// 权限规则变更
    PermissionChanged(PathBuf),
    /// 消息总线地址变更
    BusAddressChanged(String),
    /// 通用文件变更
    FileChanged(PathBuf),
}

/// 配置监听器
///
/// 监听配置文件变更，通过 channel 发送事件。
pub struct ConfigWatcher {
    /// 监听路径
    watch_path: PathBuf,
    /// 事件发送端
    event_tx: mpsc::Sender<ConfigChangeEvent>,
    /// notify watcher
    _watcher: Arc<notify::RecommendedWatcher>,
}

impl ConfigWatcher {
    /// 创建配置监听器
    ///
    /// # 参数
    /// - watch_path: 监听的配置文件路径
    /// - buffer_size: 事件 channel 缓冲区大小
    pub fn new(watch_path: PathBuf, buffer_size: usize) -> Result<(Self, mpsc::Receiver<ConfigChangeEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(buffer_size);

        let tx = event_tx.clone();
        let path_clone = watch_path.clone();

        // 创建 notify watcher
        let mut watcher = notify::recommended_watcher(move |res: std::result::Result<Event, notify::Error>| {
            if let Ok(event) = res {
                for path in &event.paths {
                    let change_event = classify_change(path, &path_clone);
                    if let Err(e) = tx.blocking_send(change_event) {
                        warn!(error = %e, "配置变更事件发送失败（接收端可能已关闭）");
                    }
                }
            }
        })?;

        // 开始监听
        watcher.watch(&watch_path, RecursiveMode::NonRecursive)?;
        info!(path = ?watch_path, "配置文件监听已启动");

        Ok((
            Self {
                watch_path,
                event_tx,
                _watcher: Arc::new(watcher),
            },
            event_rx,
        ))
    }

    /// 获取监听路径
    pub fn watch_path(&self) -> &Path {
        &self.watch_path
    }

    /// 手动发送变更事件
    pub async fn send_event(&self, event: ConfigChangeEvent) -> Result<()> {
        self.event_tx.send(event).await
            .map_err(|e| anyhow::anyhow!("发送配置变更事件失败：{}", e))
    }
}

/// 分类配置变更
///
/// 根据文件路径判断变更类型
fn classify_change(path: &Path, _watch_path: &Path) -> ConfigChangeEvent {
    let path_str = path.to_string_lossy();

    if path_str.contains("provider") {
        ConfigChangeEvent::ProviderChanged(path.to_path_buf())
    } else if path_str.contains("permission") {
        ConfigChangeEvent::PermissionChanged(path.to_path_buf())
    } else if path_str.contains("bus") {
        ConfigChangeEvent::BusAddressChanged(path_str.to_string())
    } else {
        ConfigChangeEvent::FileChanged(path.to_path_buf())
    }
}