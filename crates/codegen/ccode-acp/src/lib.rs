//! # ccode-acp
//!
//! ACP（Agent Communication Protocol）客户端，桥接 IDE/stdio 与消息总线。
//!
//! | 核心能力 | 说明 |
//! |---|---|
//! | 协议解析 | ACP 消息序列化与反序列化 |
//! | IDE 桥接 | 通过 stdio 与 IDE 双向通信 |
//! | 消息总线 | Agent/Client 通道与 Gateway 路由 |

mod channel;
mod common;
mod gateway;
mod line_reader;
mod message;
mod normalize;
mod stdin_reader;

pub use self::{
    channel::{AcpAgentChannel, AcpChannel, AcpClientChannel, acp_channels, acp_send},
    common::{
        AcpAgentRx, AcpAgentTx, AcpChannelFailure, AcpClientRx, AcpClientTx, AcpResult, AcpRxo,
        AcpTxo, acp_channel_failure, acp_internal_error,
    },
    gateway::{
        AcpAgentGatewayReceiver, AcpAgentGatewaySender, AcpClientGatewayReceiver,
        AcpClientGatewaySender, AcpGatewayReceiver, AcpGatewaySender, acp_gateway,
    },
    message::{
        AcpAgentMessage, AcpAgentMessageBox, AcpAgentMessageGeneric, AcpArgs, AcpArgsBox,
        AcpClientMessage, AcpClientMessageBox, AcpClientMessageGeneric, AcpMethod, AcpRequest,
        AcpSide, Boxed, StorageMarker, Unboxed,
    },
};

pub use self::line_reader::LineBufferedRead;
pub use self::stdin_reader::spawn_stdin_line_reader;

#[doc(hidden)]
pub use self::common::compact_json;
