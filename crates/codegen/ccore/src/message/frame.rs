//! 3 帧消息编解码
//!
//! 帧格式：
//! Frame 1: topic (UTF-8 string)
//! Frame 2: header (MessagePack) — msg_id, timestamp, src_node, reply_to
//! Frame 3: payload (MessagePack) — 业务数据

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::Utc;

/// 消息帧：3 部分组成
#[derive(Debug, Clone)]
pub struct Message {
    pub topic: super::Topic,
    pub header: MessageHeader,
    pub payload: Vec<u8>,
}

/// 消息头
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    /// 消息唯一 ID
    pub msg_id: String,
    /// 发送时间戳 (ISO 8601)
    pub timestamp: String,
    /// 发送方 Node ID
    pub src_node: String,
    /// 回复目标 msg_id（用于 REQ/REP 关联）
    pub reply_to: Option<String>,
    /// 序列号（用于顺序检查，每个 Node 独立递增）
    #[serde(default)]
    pub sequence: u64,
    /// 是否需要 ACK 确认（用于关键控制面消息）
    #[serde(default)]
    pub requires_ack: bool,
}

/// 消息帧编解码器
pub struct FrameCodec;

impl FrameCodec {
    /// 创建新消息（自动序列号）
    pub fn new_message(
        topic: super::Topic,
        src_node: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<Message> {
        Self::new_message_with_sequence(topic, src_node, payload, 0)
    }

    /// 创建新消息（指定序列号）
    pub fn new_message_with_sequence(
        topic: super::Topic,
        src_node: impl Into<String>,
        payload: &impl Serialize,
        sequence: u64,
    ) -> Result<Message> {
        let header = MessageHeader {
            msg_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            src_node: src_node.into(),
            reply_to: None,
            sequence,
            requires_ack: false,
        };
        let payload = rmp_serde::to_vec(payload)?;
        Ok(Message { topic, header, payload })
    }

    /// 创建回复消息（自动序列号）
    pub fn new_reply(
        topic: super::Topic,
        src_node: impl Into<String>,
        reply_to_msg_id: impl Into<String>,
        payload: &impl Serialize,
    ) -> Result<Message> {
        Self::new_reply_with_sequence(topic, src_node, reply_to_msg_id, payload, 0)
    }

    /// 创建回复消息（指定序列号）
    pub fn new_reply_with_sequence(
        topic: super::Topic,
        src_node: impl Into<String>,
        reply_to_msg_id: impl Into<String>,
        payload: &impl Serialize,
        sequence: u64,
    ) -> Result<Message> {
        let header = MessageHeader {
            msg_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            src_node: src_node.into(),
            reply_to: Some(reply_to_msg_id.into()),
            sequence,
            requires_ack: false,
        };
        let payload = rmp_serde::to_vec(payload)?;
        Ok(Message { topic, header, payload })
    }

    /// 将 3 帧编码为字节向量
    pub fn encode(msg: &Message) -> Result<Vec<Vec<u8>>> {
        let frame1 = msg.topic.as_str().as_bytes().to_vec();
        let frame2 = rmp_serde::to_vec(&msg.header)?;
        let frame3 = msg.payload.clone();
        Ok(vec![frame1, frame2, frame3])
    }

    /// 从字节向量解码为 3 帧
    pub fn decode(frames: &[Vec<u8>]) -> Result<Message> {
        if frames.len() != 3 {
            return Err(anyhow!("消息帧数量错误：期望 3，实际 {}", frames.len()));
        }
        let topic_str = String::from_utf8(frames[0].clone())?;
        let topic = super::Topic::new(topic_str);
        let header: MessageHeader = rmp_serde::from_read(&frames[1][..])?;
        let payload = frames[2].clone();
        Ok(Message { topic, header, payload })
    }

    /// 解码 payload 为具体类型
    pub fn decode_payload<T: serde::de::DeserializeOwned>(msg: &Message) -> Result<T> {
        Ok(rmp_serde::from_read(&msg.payload[..])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestPayload {
        text: String,
        value: i32,
    }

    #[test]
    fn test_encode_decode_roundtrip() {
        let topic = super::super::Topic::sys_heartbeat();
        let payload = TestPayload {
            text: "hello".into(),
            value: 42,
        };
        let msg = FrameCodec::new_message(topic, "node-1", &payload).unwrap();
        let frames = FrameCodec::encode(&msg).unwrap();
        let decoded = FrameCodec::decode(&frames).unwrap();
        assert_eq!(decoded.topic, msg.topic);
        assert_eq!(decoded.header.src_node, "node-1");
        let decoded_payload: TestPayload = FrameCodec::decode_payload(&decoded).unwrap();
        assert_eq!(decoded_payload, payload);
    }

    #[test]
    fn test_reply_message() {
        let topic = super::super::Topic::state_response();
        let msg = FrameCodec::new_reply(topic, "state-1", "original-msg-id", &json!({"ok": true})).unwrap();
        assert_eq!(msg.header.reply_to, Some("original-msg-id".into()));
    }
}
