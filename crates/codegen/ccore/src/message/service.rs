//! ROS 风格的 Service 机制
//!
//! 与 Topic 的异步发布/订阅不同，Service 是同步的请求/响应模式。
//!
//! ROS 1 风格实现（数据面点对点）：
//! - 注册时：Service Provider 告知 Kernel 自己的 REP socket 地址
//! - 发现时：Client 查询 Kernel 获取 Provider 的 REP 地址
//! - 调用时：Client 直接 REQ 连接 Provider 的 REP socket（不经 Kernel）
//!
//! Topic 命名规范（控制面）：
//! - 服务注册：service/{service_name}/register
//! - 服务发现：service/{service_name}/lookup

use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

use crate::message::frame::FrameCodec;
use crate::message::{Message, Topic};
use crate::node::transport::NodeTransportHandle;

/// Service 请求 ID
pub type ServiceRequestId = String;

/// Service 客户端
///
/// 发送请求并等待响应（类似 ROS 的 ServiceClient）
pub struct ServiceClient {
    /// 调用方的 Node ID
    node_id: String,
    /// 正在等待响应的请求
    pending_requests: Arc<Mutex<HashMap<ServiceRequestId, oneshot::Sender<Message>>>>,
    /// 传输层句柄
    transport: NodeTransportHandle,
}

impl ServiceClient {
    pub fn new(node_id: String, transport: NodeTransportHandle) -> Self {
        Self {
            node_id,
            pending_requests: Arc::new(Mutex::new(HashMap::new())),
            transport,
        }
    }

    /// 调用远程服务并等待响应
    ///
    /// 类似 ROS 的 service.call(request)
    pub async fn call(
        &self,
        service_name: &str,
        request: &impl serde::Serialize,
    ) -> Result<Message> {
        let request_id = Uuid::new_v4().to_string();
        let topic = Topic::new(format!("service/{}/request", service_name));

        // 创建 oneshot 通道用于等待响应
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending_requests.lock().await;
            pending.insert(request_id.clone(), tx);
        }

        // 发送请求（包含 request_id 用于关联响应）
        let payload = serde_json::json!({
            "request_id": request_id,
            "data": request,
        });
        let msg = FrameCodec::new_message(topic, &self.node_id, &payload)?;
        self.transport.send_message(&msg).await?;

        // 等待响应（带超时）
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            rx,
        )
        .await
        .map_err(|_| anyhow::anyhow!("Service 调用超时：{}", service_name))?
        .map_err(|_| anyhow::anyhow!("Service 调用被取消：{}", service_name))?;

        Ok(response)
    }

    /// 处理收到的 Service 响应
    ///
    /// 当收到 service/{name}/response 消息时调用此方法
    pub async fn handle_response(&self, msg: &Message) -> Result<()> {
        let payload: serde_json::Value = FrameCodec::decode_payload(msg)?;
        let request_id = payload["request_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Service 响应缺少 request_id"))?
            .to_string();

        let mut pending = self.pending_requests.lock().await;
        if let Some(tx) = pending.remove(&request_id) {
            let _ = tx.send(msg.clone());
        } else {
            tracing::warn!("收到未知 request_id 的 Service 响应：{}", request_id);
        }

        Ok(())
    }
}

/// Service 服务端
///
/// 接收请求并发送响应（类似 ROS 的 ServiceServer）
#[async_trait::async_trait]
pub trait Service: Send + Sync {
    /// 服务名称
    fn service_name(&self) -> &str;

    /// 处理请求并返回响应
    async fn handle_request(&self, request: &Message) -> Result<Message>;
}


