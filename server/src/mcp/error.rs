use thiserror::Error;

#[derive(Error, Debug)]
pub enum McpError {
    #[error("连接错误: {0}")]
    ConnectionError(String),

    #[error("协议错误: {0}")]
    ProtocolError(String),

    #[error("JSON-RPC错误: {code} - {message}")]
    JsonRpcError { code: i32, message: String },

    #[error("序列化错误: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("WebSocket错误: {0}")]
    WebSocketError(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("IO错误: {0}")]
    IoError(#[from] std::io::Error),

    #[error("超时错误")]
    TimeoutError,

    #[error("服务器未初始化")]
    NotInitialized,

    #[error("工具未找到: {0}")]
    ToolNotFound(String),

    #[error("无效参数: {0}")]
    InvalidArguments(String),

    #[error("认证失败")]
    AuthenticationFailed,

    #[error("权限不足")]
    PermissionDenied,

    #[error("资源不存在: {0}")]
    ResourceNotFound(String),

    #[error("内部错误: {0}")]
    InternalError(String),
}

impl From<McpError> for crate::function_callback::FunctionCallbackError {
    fn from(err: McpError) -> Self {
        crate::function_callback::FunctionCallbackError::Other(err.to_string())
    }
}
