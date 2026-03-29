//! WebSocket消息适配器
//! 基于actix-ws的WebSocket消息类型

use bytes::Bytes;
use std::fmt;

/// 基于actix-ws的WebSocket消息类型
#[derive(Debug, Clone)]
pub enum WsMessage {
    /// 文本消息
    Text(String),
    /// 二进制消息
    Binary(Bytes),
    /// Ping消息
    Ping(Bytes),
    /// Pong消息
    Pong(Bytes),
    /// 关闭消息
    Close(Option<WsCloseFrame>),
}

/// WebSocket关闭帧
#[derive(Debug, Clone)]
pub struct WsCloseFrame {
    pub code: u16,
    pub reason: String,
}

impl fmt::Display for WsMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsMessage::Text(text) => write!(f, "Text({})", text.chars().take(50).collect::<String>()),
            WsMessage::Binary(data) => write!(f, "Binary({} bytes)", data.len()),
            WsMessage::Ping(data) => write!(f, "Ping({} bytes)", data.len()),
            WsMessage::Pong(data) => write!(f, "Pong({} bytes)", data.len()),
            WsMessage::Close(frame) => {
                if let Some(frame) = frame {
                    write!(f, "Close({}, {})", frame.code, frame.reason)
                } else {
                    write!(f, "Close")
                }
            },
        }
    }
}

// 从actix-ws Message转换
impl From<actix_ws::Message> for WsMessage {
    fn from(msg: actix_ws::Message) -> Self {
        match msg {
            actix_ws::Message::Text(text) => WsMessage::Text(text.to_string()),
            actix_ws::Message::Binary(data) => WsMessage::Binary(data),
            actix_ws::Message::Ping(data) => WsMessage::Ping(data),
            actix_ws::Message::Pong(data) => WsMessage::Pong(data),
            actix_ws::Message::Close(reason) => {
                let close_frame = reason.map(|r| WsCloseFrame { code: r.code.into(), reason: r.description.unwrap_or_default().to_string() });
                WsMessage::Close(close_frame)
            },
            actix_ws::Message::Continuation(_) => {
                // actix-ws特有的continuation frame，转换为空的二进制消息
                WsMessage::Binary(Bytes::new())
            },
            actix_ws::Message::Nop => {
                // actix-ws特有的no-op消息，转换为空的ping
                WsMessage::Ping(Bytes::new())
            },
        }
    }
}

// 转换到actix-ws Message
impl From<WsMessage> for actix_ws::Message {
    fn from(msg: WsMessage) -> Self {
        match msg {
            WsMessage::Text(text) => actix_ws::Message::Text(text.into()),
            WsMessage::Binary(data) => actix_ws::Message::Binary(data),
            WsMessage::Ping(data) => actix_ws::Message::Ping(data),
            WsMessage::Pong(data) => actix_ws::Message::Pong(data),
            WsMessage::Close(frame) => {
                let close_reason = frame.map(|f| actix_ws::CloseReason { code: actix_ws::CloseCode::from(f.code), description: Some(f.reason) });
                actix_ws::Message::Close(close_reason)
            },
        }
    }
}

// 为了方便使用，提供从String和字节数组的直接转换
impl From<String> for WsMessage {
    fn from(text: String) -> Self {
        WsMessage::Text(text)
    }
}

impl From<&str> for WsMessage {
    fn from(text: &str) -> Self {
        WsMessage::Text(text.to_string())
    }
}

impl From<Vec<u8>> for WsMessage {
    fn from(data: Vec<u8>) -> Self {
        WsMessage::Binary(Bytes::from(data))
    }
}

impl From<Bytes> for WsMessage {
    fn from(data: Bytes) -> Self {
        WsMessage::Binary(data)
    }
}
