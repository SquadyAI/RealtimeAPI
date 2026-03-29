//! Edge TTS 类型定义

use thiserror::Error;

/// Edge TTS 错误类型
#[derive(Error, Debug)]
pub enum EdgeTtsError {
    #[error("WebSocket 连接错误: {0}")]
    WebSocket(String),

    #[error("MP3 解码错误: {0}")]
    Mp3Decode(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("不支持的语言: {0}")]
    UnsupportedLanguage(String),

    #[error("连接超时")]
    ConnectionTimeout,

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("其他错误: {0}")]
    Other(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for EdgeTtsError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        EdgeTtsError::WebSocket(e.to_string())
    }
}
