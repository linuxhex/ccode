//! 配置重载器
//!
//! 监听配置变更事件，重新加载配置并广播通知。

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use super::watcher::ConfigChangeEvent;
use crate::config::CcodeConfig;

/// 配置重载器
///
/// 接收配置变更事件，重新加载配置，并广播通知到消息总线。
pub struct ConfigReloader {
    /// 当前配置
    config: Arc<RwLock<CcodeConfig>>,
    /// 配置文件路径
    config_path: PathBuf,
    /// 配置变更通知发送端（向 Kernel 主循环发送变更类型，由 Kernel 广播到所有 Node）
    notify_tx: mpsc::Sender<String>,
}

impl ConfigReloader {
    /// 创建配置重载器
    ///
    /// # 参数
    /// - config: 共享配置（与 Kernel 共享，重载后 Kernel 也能读到最新配置）
    /// - config_path: 配置文件路径（用于重新加载）
    /// - notify_tx: 配置变更通知发送端，Kernel 通过此通道接收变更类型并广播
    pub fn new(
        config: Arc<RwLock<CcodeConfig>>,
        config_path: PathBuf,
        notify_tx: mpsc::Sender<String>,
    ) -> Self {
        Self { config, config_path, notify_tx }
    }

    /// 启动配置重载循环
    ///
    /// 接收配置变更事件，重新加载配置
    pub async fn run(self, mut event_rx: mpsc::Receiver<ConfigChangeEvent>) {
        info!("配置重载器已启动");

        while let Some(event) = event_rx.recv().await {
            match self.handle_event(event).await {
                Ok(()) => info!("配置重载成功"),
                Err(e) => error!("配置重载失败：{}", e),
            }
        }

        warn!("配置重载器已停止");
    }

    /// 处理配置变更事件
    async fn handle_event(&self, event: ConfigChangeEvent) -> Result<()> {
        match event {
            ConfigChangeEvent::ProviderChanged(path) => {
                info!(path = ?path, "Provider 配置变更，重新加载");
                self.reload_config().await?;
                self.broadcast_change("provider").await;
            }
            ConfigChangeEvent::PermissionChanged(path) => {
                info!(path = ?path, "权限规则变更，重新加载");
                self.reload_config().await?;
                self.broadcast_change("permission").await;
            }
            ConfigChangeEvent::BusAddressChanged(addr) => {
                info!(addr = %addr, "消息总线地址变更");
                self.broadcast_change("bus_address").await;
            }
            ConfigChangeEvent::FileChanged(path) => {
                info!(path = ?path, "配置文件变更，重新加载");
                self.reload_config().await?;
                self.broadcast_change("all").await;
            }
        }
        Ok(())
    }

    /// 重新加载配置
    async fn reload_config(&self) -> Result<()> {
        let new_config = CcodeConfig::load_from_file(&self.config_path)?;
        let mut config = self.config.write().await;
        *config = new_config;
        info!("配置已重新加载");
        Ok(())
    }

    /// 广播配置变更通知
    ///
    /// 通过 notify_tx 向 Kernel 主循环发送变更类型，
    /// 由 Kernel 构造 sys/config_change 消息并广播到所有 Node。
    async fn broadcast_change(&self, change_type: &str) {
        info!(change_type = %change_type, "广播配置变更通知");
        if let Err(e) = self.notify_tx.send(change_type.to_string()).await {
            warn!(change_type = %change_type, error = %e, "发送配置变更通知失败（Kernel 主循环可能已退出）");
        }
    }
}