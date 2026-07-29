//! 内存系统生命周期集成测试

use ccore::memory::storage::{MemoryStorage, MemoryScope};

#[tokio::test]
async fn test_memory_save_and_load() {
    let temp_dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::new(temp_dir.path().to_path_buf(), "/test/project");
    storage.ensure_initialized().await.unwrap();

    // 写入
    storage.append(MemoryScope::Workspace, "# Test Memory\n\nSome knowledge here.").await.unwrap();

    // 读取
    let content = storage.read(MemoryScope::Workspace).await.unwrap();
    assert!(content.is_some());
    assert!(content.unwrap().contains("Test Memory"));
}

#[tokio::test]
async fn test_three_tier_storage() {
    let temp_dir = tempfile::tempdir().unwrap();
    let storage = MemoryStorage::new(temp_dir.path().to_path_buf(), "/test/project");
    storage.ensure_initialized().await.unwrap();

    // Global
    storage.append(MemoryScope::Global, "Global knowledge").await.unwrap();
    // Workspace
    storage.append(MemoryScope::Workspace, "Workspace knowledge").await.unwrap();
    // Session (通过 write_session 方法)
    storage.write_session("2026-07-30", "test-session", "session001", "Session knowledge").await.unwrap();

    // Verify isolation
    let global = storage.read(MemoryScope::Global).await.unwrap().unwrap();
    let workspace = storage.read(MemoryScope::Workspace).await.unwrap().unwrap();

    assert!(global.contains("Global"));
    assert!(workspace.contains("Workspace"));

    // Verify session file was written
    let sessions = storage.list_sessions().await.unwrap();
    assert!(!sessions.is_empty());
    let session_content = tokio::fs::read_to_string(&sessions[0]).await.unwrap();
    assert!(session_content.contains("Session"));
}
