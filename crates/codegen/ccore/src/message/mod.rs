//! 消息协议模块 - 3 帧消息格式、Topic 路由、MessagePack 编解码
//!
//! ROS 风格的消息总线：
//! - Topic（发布/订阅）：异步消息传递
//! - Service（请求/响应）：同步 RPC 调用
//! - Param（参数服务器）：全局共享配置

pub mod topic;
pub mod frame;
pub mod sequence;
pub mod ack;
pub mod service;
pub mod param;

pub use topic::{Topic, TopicPattern};
pub use frame::{Message, MessageHeader, FrameCodec};
pub use sequence::{SequenceManager, SequenceChecker, SequenceCheckResult, SequenceError};
pub use ack::{AckManager, AckConfig, PendingAck, create_ack_message};
pub use service::{ServiceClient, ServiceRequestId};
pub use param::{ParamServer, ParamValue, ParamChangeNotification, ParamChangeType};
