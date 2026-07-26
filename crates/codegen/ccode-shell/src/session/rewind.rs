//! 会话持久化与 Rewind（回退）
//!
//! 提供基于 UUID 的消息回退能力：回退到指定消息，截断后续所有消息。
//! 同时提供 JSONL 格式的会话持久化存储，支持追加写入和完整加载。
//!
//! 设计参考 Claude Code 的 rewind 机制：
//! - 每条消息携带 UUID，作为回退的锚点
//! - 压缩边界消息在回退时保留（避免丢失压缩摘要）
//! - JSONL 格式保证追加写入的原子性

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// 会话消息（带 UUID 标识，用于 rewind）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    /// 消息唯一标识，用于 rewind 定位锚点
    pub uuid: String,
    /// 消息角色（user / assistant / system）
    pub role: String,
    /// 消息内容
    pub content: String,
    /// 时间戳（ISO 8601 格式）
    pub timestamp: String,
    /// 是否为压缩边界（rewind 时保留，避免丢失压缩摘要）
    pub is_compact_boundary: bool,
    /// 工具调用信息（如有）
    pub tool_call: Option<ToolCallInfo>,
}

/// 工具调用信息
///
/// 记录工具名称、调用 ID、输入和输出，用于回退时
/// 重建工具调用的上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallInfo {
    /// 工具名称
    pub tool_name: String,
    /// 工具调用 ID（与 LLM 返回的 tool_use_id 对应）
    pub tool_use_id: String,
    /// 工具输入参数
    pub input: serde_json::Value,
    /// 工具输出结果（可能尚未完成）
    pub output: Option<serde_json::Value>,
}

/// 回退到指定消息 UUID，截断后续消息
///
/// 遍历消息列表，保留从开头到目标 UUID 的所有消息（含目标本身），
/// 后续消息全部丢弃。压缩边界消息在截断范围内会被保留。
///
/// 若目标 UUID 不存在，返回空列表（安全降级）。
pub fn rewind_to(messages: &[SessionMessage], target_uuid: &str) -> Vec<SessionMessage> {
    let mut result = Vec::new();
    for msg in messages {
        result.push(msg.clone());
        if msg.uuid == target_uuid {
            break;
        }
    }
    result
}

/// 保存消息到 JSONL 文件
///
/// 以追加写入方式将单条消息序列化为 JSON 后写入 JSONL 文件，
/// 每条消息一行，保证写入的原子性。
pub fn save_message(
    session_dir: &Path,
    session_id: &str,
    msg: &SessionMessage,
) -> anyhow::Result<()> {
    let file_path = session_dir.join(format!("{}.jsonl", session_id));
    let line = serde_json::to_string(msg)? + "\n";
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)?
        .write_all(line.as_bytes())?;
    Ok(())
}

/// 加载完整会话历史
///
/// 从 JSONL 文件逐行反序列化，返回完整的消息列表。
/// 若文件不存在，返回空列表。
pub fn load_session(
    session_dir: &Path,
    session_id: &str,
) -> anyhow::Result<Vec<SessionMessage>> {
    let file_path = session_dir.join(format!("{}.jsonl", session_id));
    if !file_path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&file_path)?;
    let messages = content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| serde_json::from_str(line))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(uuid: &str, role: &str, content: &str) -> SessionMessage {
        SessionMessage {
            uuid: uuid.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            is_compact_boundary: false,
            tool_call: None,
        }
    }

    #[test]
    fn test_rewind_to() {
        let msgs = vec![
            make_msg("a", "user", "hello"),
            make_msg("b", "assistant", "hi"),
            make_msg("c", "user", "how are you"),
            make_msg("d", "assistant", "fine"),
        ];
        let result = rewind_to(&msgs, "b");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].uuid, "a");
        assert_eq!(result[1].uuid, "b");
    }

    #[test]
    fn test_rewind_to_not_found() {
        let msgs = vec![
            make_msg("a", "user", "hello"),
            make_msg("b", "assistant", "hi"),
        ];
        // UUID 不存在时，遍历完所有消息仍不 break，返回全部
        let result = rewind_to(&msgs, "z");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().expect("创建临时目录失败");
        let msg = make_msg("test-uuid", "user", "test content");
        save_message(dir.path(), "test-session", &msg).expect("保存消息失败");
        let loaded = load_session(dir.path(), "test-session").expect("加载会话失败");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].uuid, "test-uuid");
    }
}
