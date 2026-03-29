use thiserror::Error;

/// RPC系统的错误类型
#[derive(Error, Debug)]
pub enum RpcError {
    #[error("WebSocket通信错误: {0}")]
    WebSocketError(String),

    #[error("会话错误: {0}")]
    SessionError(String),

    #[error("音频处理错误: {0}")]
    AudioError(String),

    #[error("配置错误: {0}")]
    ConfigError(String),

    #[error("超时错误: {message}, 超时时间: {timeout_ms}ms")]
    TimeoutError { message: String, timeout_ms: u64 },

    #[error("资源不足: {resource}")]
    ResourceExhausted { resource: String },

    #[error("序列化错误: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("IO错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("系统内部错误: {0}")]
    InternalError(String),
}

impl RpcError {
    pub fn session_not_found(session_id: &str) -> Self {
        Self::SessionError(format!("会话未找到: {}", session_id))
    }

    pub fn session_limit_exceeded(limit: usize) -> Self {
        Self::ResourceExhausted { resource: format!("会话数量超过限制: {}", limit) }
    }

    pub fn audio_parse_error(reason: &str) -> Self {
        Self::AudioError(format!("音频数据解析失败: {}", reason))
    }

    pub fn websocket_connection_error(reason: &str) -> Self {
        Self::WebSocketError(format!("WebSocket连接失败: {}", reason))
    }

    pub fn timeout(operation: &str, timeout_ms: u64) -> Self {
        Self::TimeoutError { message: operation.to_string(), timeout_ms }
    }
}

pub type RpcResult<T> = Result<T, RpcError>;

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for RpcError {
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        RpcError::InternalError(format!("消息发送失败: {}", err))
    }
}
