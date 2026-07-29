//! 文件存储后端实现
//!
//! 基于 JSON 文件的持久化存储，支持原子写入。

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use super::storage::StorageBackend;

/// 文件存储后端
///
/// 将数据保存为 JSON 文件，支持原子写入（先写临时文件再重命名）。
pub struct FileStorage {
    /// 存储目录根路径
    base_dir: PathBuf,
}

impl FileStorage {
    /// 创建文件存储后端
    ///
    /// # 参数
    /// - base_dir: 存储目录根路径
    ///
    /// # 返回
    /// - Ok(storage) 创建成功
    /// - Err(e) 创建失败（目录不存在且无法创建）
    pub async fn new(base_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&base_dir)
            .await
            .with_context(|| format!("无法创建存储目录：{:?}", base_dir))?;
        Ok(Self { base_dir })
    }

    /// 获取指定 key 的文件路径
    fn key_to_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(format!("{}.json", key))
    }
}

#[async_trait]
impl StorageBackend for FileStorage {
    async fn save(&self, key: &str, data: &[u8]) -> Result<()> {
        let path = self.key_to_path(key);
        let temp_path = path.with_extension("tmp");

        // 原子写入：先写临时文件
        let mut file = fs::File::create(&temp_path)
            .await
            .with_context(|| format!("无法创建临时文件：{:?}", temp_path))?;
        file.write_all(data).await
            .with_context(|| "无法写入数据到临时文件")?;
        file.sync_all().await
            .with_context(|| "无法同步临时文件")?;

        // 原子重命名
        fs::rename(&temp_path, &path)
            .await
            .with_context(|| format!("无法重命名临时文件：{:?} -> {:?}", temp_path, path))?;

        tracing::debug!(key = %key, path = ?path, "数据已保存");
        Ok(())
    }

    async fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.key_to_path(key);
        match fs::read(&path).await {
            Ok(data) => {
                tracing::debug!(key = %key, path = ?path, size = data.len(), "数据已加载");
                Ok(Some(data))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(key = %key, "数据不存在");
                Ok(None)
            }
            Err(e) => {
                Err(e).with_context(|| format!("无法读取文件：{:?}", path))
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.key_to_path(key);
        match fs::remove_file(&path).await {
            Ok(_) => {
                tracing::debug!(key = %key, path = ?path, "数据已删除");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(key = %key, "数据不存在，无需删除");
                Ok(())
            }
            Err(e) => {
                Err(e).with_context(|| format!("无法删除文件：{:?}", path))
            }
        }
    }
}