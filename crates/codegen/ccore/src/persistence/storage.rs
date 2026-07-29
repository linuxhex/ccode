//! 存储后端 trait 定义
//!
//! 定义统一的存储接口，支持多种后端实现（文件/Redis/S3）。

use anyhow::Result;
use async_trait::async_trait;

/// 存储后端 trait：定义 save/load/delete 接口
///
/// 所有存储后端必须实现此 trait，以支持持久化模块的统一调用。
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// 保存数据到指定 key
    ///
    /// # 参数
    /// - key: 存储键（如 session_id）
    /// - data: 序列化后的数据
    ///
    /// # 返回
    /// - Ok(()) 保存成功
    /// - Err(e) 保存失败
    async fn save(&self, key: &str, data: &[u8]) -> Result<()>;

    /// 从指定 key 加载数据
    ///
    /// # 参数
    /// - key: 存储键
    ///
    /// # 返回
    /// - Ok(Some(data)) 数据存在
    /// - Ok(None) 数据不存在
    /// - Err(e) 加载失败
    async fn load(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// 删除指定 key 的数据
    ///
    /// # 参数
    /// - key: 存储键
    ///
    /// # 返回
    /// - Ok(()) 删除成功
    /// - Err(e) 删除失败
    async fn delete(&self, key: &str) -> Result<()>;
}