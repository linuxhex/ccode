//! 持久化模块：会话状态、Agent 状态的保存与恢复
//!
//! 提供可插拔的存储后端（文件/Redis/S3），支持异步持久化。

pub mod storage;
pub mod session;
pub mod state;
pub mod file_storage;

pub use storage::StorageBackend;
pub use session::{SessionPersister, SessionPersistMeta};
pub use state::{StatePersister, LoopStateSnapshot};
pub use file_storage::FileStorage;