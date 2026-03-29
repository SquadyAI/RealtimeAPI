use super::*;
use crate::mcp::error::McpError;
use crate::mcp::protocol::*;
use futures_util::{SinkExt, StreamExt};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, error, info};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// MCP 客户端包装器，支持多种协议
#[derive(Debug, Clone)]
pub enum McpClientWrapper {
    /// HTTP MCP 客户端（支持控制流程）
    Http {
        client: Arc<crate::mcp::HttpMcpClient>,
        config: crate::mcp::McpServerConfig,
    },
    /// WebSocket MCP 客户端（传统模式）
    WebSocket {
        manager: Arc<crate::mcp::McpManager>,
        config: crate::mcp::McpServerConfig,
    },
}

impl McpClientWrapper {
    /// 获取 MCP server 的 endpoint
    pub fn endpoint(&self) -> &str {
        match self {
            Self::Http { config, .. } => &config.endpoint,
            Self::WebSocket { config, .. } => &config.endpoint,
        }
    }

    /// 获取 MCP server 的配置
    pub fn config(&self) -> &crate::mcp::McpServerConfig {
        match self {
            Self::Http { config, .. } => config,
            Self::WebSocket { config, .. } => config,
        }
    }

    /// 检查是否为 HTTP 协议
    pub fn is_http(&self) -> bool {
        matches!(self, Self::Http { .. })
    }

    /// 检查是否为 WebSocket 协议
    pub fn is_websocket(&self) -> bool {
        matches!(self, Self::WebSocket { .. })
    }
}

/// MCP客户端
#[derive(Debug)]
pub struct McpClient {
    config: McpServerConfig,
    connection_state: Arc<RwLock<McpConnectionState>>,
    ws_stream: Arc<Mutex<Option<WsStream>>>,
    pending_requests: Arc<RwLock<FxHashMap<String, oneshot::Sender<McpResponse>>>>,
    tools_cache: Arc<RwLock<(Vec<crate::mcp::McpTool>, Instant)>>,
    server_info: Arc<RwLock<Option<ServerInfo>>>,
    request_id_counter: Arc<Mutex<u64>>,
}

impl McpClient {
    /// 创建新的MCP客户端
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            connection_state: Arc::new(RwLock::new(McpConnectionState::Disconnected)),
            ws_stream: Arc::new(Mutex::new(None)),
            pending_requests: Arc::new(RwLock::new(FxHashMap::default())),
            tools_cache: Arc::new(RwLock::new((Vec::new(), Instant::now()))),
            server_info: Arc::new(RwLock::new(None)),
            request_id_counter: Arc::new(Mutex::new(0)),
        }
    }

    /// 连接到MCP服务器
    pub async fn connect(&self) -> Result<(), McpError> {
        info!("连接到MCP服务器: {}", self.config.endpoint);

        // 设置连接状态
        *self.connection_state.write().await = McpConnectionState::Connecting;

        // 建立WebSocket连接
        let ws_stream = match timeout(
            Duration::from_secs(self.config.timeout_secs),
            connect_async(&self.config.endpoint),
        )
        .await
        {
            Ok(Ok((stream, _))) => stream,
            Ok(Err(e)) => {
                let err_msg = format!("WebSocket连接失败: {}", e);
                error!("{}", err_msg);
                *self.connection_state.write().await = McpConnectionState::Error(err_msg.clone());
                return Err(McpError::ConnectionError(err_msg));
            },
            Err(_) => {
                let err_msg = "连接超时".to_string();
                error!("{}", err_msg);
                *self.connection_state.write().await = McpConnectionState::Error(err_msg.clone());
                return Err(McpError::TimeoutError);
            },
        };

        // 存储连接
        *self.ws_stream.lock().await = Some(ws_stream);

        // 启动消息处理任务
        self.start_message_handler().await;

        // 执行初始化握手
        self.initialize().await?;

        // 设置连接状态为已连接
        *self.connection_state.write().await = McpConnectionState::Connected;
        info!("✅ MCP服务器连接成功");

        Ok(())
    }

    /// 断开连接
    pub async fn disconnect(&self) -> Result<(), McpError> {
        info!("断开MCP服务器连接");

        // 关闭WebSocket连接
        if let Some(mut stream) = self.ws_stream.lock().await.take() {
            let _ = stream.close(None).await;
        }

        // 清理待处理请求
        let mut pending = self.pending_requests.write().await;
        let senders: Vec<_> = pending.drain().collect();
        drop(pending); // 释放锁

        for (_, sender) in senders {
            let _ = sender.send(McpResponse {
                jsonrpc: JSONRPC_VERSION.to_string(),
                id: serde_json::Value::Null,
                result: None,
                error: Some(crate::mcp::protocol::McpError { code: -1, message: "连接已断开".to_string(), data: None }),
            });
        }

        // 更新状态
        *self.connection_state.write().await = McpConnectionState::Disconnected;
        info!("✅ MCP服务器连接已断开");

        Ok(())
    }

    /// 获取连接状态
    pub async fn get_connection_state(&self) -> McpConnectionState {
        self.connection_state.read().await.clone()
    }

    /// 获取可用工具列表
    pub async fn get_tools(&self) -> Result<Vec<crate::mcp::McpTool>, McpError> {
        // 检查缓存
        {
            let cache = self.tools_cache.read().await;
            let (tools, cached_at) = &*cache;
            if !tools.is_empty() && cached_at.elapsed() < Duration::from_secs(self.config.tool_cache_ttl_secs) {
                debug!("从缓存返回工具列表: {} 个工具", tools.len());
                return Ok(tools.clone());
            }
        }

        // 从服务器获取工具列表
        info!("从MCP服务器获取工具列表");
        let request_id = self.generate_request_id().await;
        let request = McpRequest::list_tools(serde_json::Value::String(request_id.clone()), None);

        let response = self.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(McpError::JsonRpcError { code: error.code, message: error.message });
        }

        let result = response.result.ok_or(McpError::ProtocolError("缺少结果".to_string()))?;
        let list_result: ListToolsResult = serde_json::from_value(result)?;

        // 转换为内部格式
        let tools: Vec<crate::mcp::McpTool> = list_result
            .tools
            .into_iter()
            .map(|tool| crate::mcp::McpTool { name: tool.name, description: tool.description, input_schema: tool.input_schema })
            .collect();

        // 更新缓存
        {
            let mut cache = self.tools_cache.write().await;
            *cache = (tools.clone(), Instant::now());
        }

        info!("✅ 获取到 {} 个MCP工具", tools.len());
        Ok(tools)
    }

    /// 调用工具
    pub async fn call_tool(&self, session_id: &str, name: &str, arguments: Option<serde_json::Value>) -> Result<crate::mcp::McpToolResult, McpError> {
        // 🔧 添加详细的入参日志记录
        info!(
            "🔗 调用 WebSocket MCP 工具: session_id={}, tool_name={}, endpoint={}",
            session_id, name, self.config.endpoint
        );
        debug!(
            "📝 WebSocket MCP 工具调用参数: session_id={}, tool_name={}, arguments={:?}",
            session_id, name, arguments
        );

        let request_id = self.generate_request_id().await;
        let request = McpRequest::call_tool(serde_json::Value::String(request_id.clone()), name.to_string(), arguments);

        let response = self.send_request(request).await?;

        if let Some(error) = response.error {
            error!(
                "❌ WebSocket MCP 工具调用失败: session_id={}, tool_name={}, error_code={}, error_message={}",
                session_id, name, error.code, error.message
            );
            return Err(McpError::JsonRpcError { code: error.code, message: error.message });
        }

        let result = response.result.ok_or(McpError::ProtocolError("缺少结果".to_string()))?;
        let call_result: CallToolResult = serde_json::from_value(result)?;
        // 克隆 call_result 以避免部分移动
        let call_result_clone = call_result.clone();

        // 转换为内部格式
        let content: Vec<crate::mcp::McpContent> = call_result
            .content
            .into_iter()
            .map(|content| match content {
                Content::Text { text } => crate::mcp::McpContent::Text { text },
                Content::Image { data, mime_type } => crate::mcp::McpContent::Image { data, mime_type },
                Content::Resource { resource } => crate::mcp::McpContent::Resource {
                    resource: crate::mcp::McpResourceReference { uri: resource.uri, text: resource.text },
                },
            })
            .collect();

        let result = crate::mcp::McpToolResult { content, is_error: call_result.is_error };

        // 🔍 打印MCP工具调用的原始结果
        info!(
            "📋 WebSocket MCP 工具调用原始结果 - session_id: {}, tool_name: {}, endpoint: {}",
            session_id, name, self.config.endpoint
        );
        info!(
            "📋 WebSocket MCP 原始call_result: {}",
            serde_json::to_string_pretty(&call_result_clone).unwrap_or_else(|_| format!("{:?}", call_result_clone))
        );
        info!(
            "📋 WebSocket MCP 转换后result: {}",
            serde_json::to_string_pretty(&result).unwrap_or_else(|_| format!("{:?}", result))
        );

        info!(
            "✅ WebSocket MCP 工具调用完成: session_id={}, tool_name={}, is_error={}, content_count={}",
            session_id,
            name,
            result.is_error,
            result.content.len()
        );
        debug!(
            "📝 WebSocket MCP 工具调用结果: session_id={}, tool_name={}, result={:?}",
            session_id, name, result
        );

        Ok(result)
    }

    /// 执行初始化握手
    async fn initialize(&self) -> Result<(), McpError> {
        info!("初始化MCP连接");

        let request_id = self.generate_request_id().await;
        let request = McpRequest::initialize(
            serde_json::Value::String(request_id.clone()),
            "realtime-asr-llm-tts".to_string(),
            "1.0.0".to_string(),
        );

        let response = self.send_request(request).await?;

        if let Some(error) = response.error {
            return Err(McpError::JsonRpcError { code: error.code, message: error.message });
        }

        let result = response.result.ok_or(McpError::ProtocolError("缺少初始化结果".to_string()))?;
        let init_result: InitializeResult = serde_json::from_value(result)?;

        // 保存服务器信息
        *self.server_info.write().await = Some(init_result.server_info.clone());

        // 发送initialized通知
        let initialized_request = McpRequest::initialized();
        self.send_notification(initialized_request).await?;

        info!(
            "✅ MCP初始化完成 - 服务器: {} v{}",
            init_result.server_info.name, init_result.server_info.version
        );

        Ok(())
    }

    /// 发送请求并等待响应
    async fn send_request(&self, request: McpRequest) -> Result<McpResponse, McpError> {
        let request_id = match &request.id {
            serde_json::Value::String(id) => id.clone(),
            _ => return Err(McpError::ProtocolError("无效的请求ID".to_string())),
        };
        // 📝 记录请求方法名（用于后续响应与错误日志的关联）
        let request_method = request.method.clone();

        // 创建响应通道
        let (tx, rx) = oneshot::channel();

        // 注册待处理请求
        self.pending_requests.write().await.insert(request_id.clone(), tx);

        // 发送请求（附加关键信息日志）
        info!(
            "📤 发送 WebSocket MCP 请求: id={}, method={}, endpoint={}",
            request_id, request_method, self.config.endpoint
        );
        if let Some(params) = &request.params {
            debug!(
                "🧾 WebSocket MCP 请求参数: id={}, method={}, params={}",
                request_id,
                request_method,
                serde_json::to_string_pretty(params).unwrap_or_else(|_| format!("{:?}", params))
            );
        }
        // 发送请求
        self.send_message(request).await?;

        // 等待响应
        match timeout(Duration::from_secs(self.config.timeout_secs), rx).await {
            Ok(Ok(response)) => {
                // 🔍 收到响应，记录摘要日志
                let has_error = response.error.is_some();
                let has_result = response.result.is_some();
                info!(
                    "📥 收到 WebSocket MCP 响应: id={}, method={}, has_result={}, has_error={}",
                    request_id, request_method, has_result, has_error
                );
                if let Some(err) = &response.error {
                    error!(
                        "❌ WebSocket MCP 响应错误: id={}, method={}, code={}, message={}",
                        request_id, request_method, err.code, err.message
                    );
                    if let Some(data) = &err.data {
                        debug!(
                            "🧾 WebSocket MCP 错误详情: id={}, data={}",
                            request_id,
                            serde_json::to_string_pretty(data).unwrap_or_else(|_| format!("{:?}", data))
                        );
                    }
                } else if let Some(result) = &response.result {
                    debug!(
                        "🧾 WebSocket MCP 响应结果: id={}, method={}, result={}",
                        request_id,
                        request_method,
                        serde_json::to_string_pretty(result).unwrap_or_else(|_| format!("{:?}", result))
                    );
                }
                Ok(response)
            },
            Ok(Err(_)) => {
                error!("❌ WebSocket MCP 响应通道关闭: id={}, method={}", request_id, request_method);
                Err(McpError::InternalError("响应通道关闭".to_string()))
            },
            Err(_) => {
                // 清理超时请求
                self.pending_requests.write().await.remove(&request_id);
                error!(
                    "⏳ WebSocket MCP 请求超时: id={}, method={}, timeout_secs={}",
                    request_id, request_method, self.config.timeout_secs
                );
                Err(McpError::TimeoutError)
            },
        }
    }

    /// 发送通知（不等待响应）
    async fn send_notification(&self, request: McpRequest) -> Result<(), McpError> {
        self.send_message(request).await
    }

    /// 发送消息到WebSocket
    async fn send_message(&self, message: McpRequest) -> Result<(), McpError> {
        let json = serde_json::to_string(&message)?;
        debug!("发送MCP消息: {}", json);

        // 针对工具调用添加更详细的日志（不包含敏感信息）
        if message.method == crate::mcp::protocol::METHOD_CALL_TOOL
            && let Some(params) = &message.params
        {
            // 尝试解析为 CallToolParams，用于提取工具名与参数
            if let Ok(parsed) = serde_json::from_value::<crate::mcp::protocol::CallToolParams>(params.clone()) {
                info!(
                    "🛠️ WebSocket MCP 工具调用请求: id={:?}, tool_name={}, has_arguments={}",
                    message.id,
                    parsed.name,
                    parsed.arguments.as_ref().map(|v| !v.is_null()).unwrap_or(false)
                );
                if let Some(args) = &parsed.arguments {
                    debug!(
                        "🧾 WebSocket MCP 工具调用参数: id={:?}, tool_name={}, arguments={}",
                        message.id,
                        parsed.name,
                        serde_json::to_string_pretty(args).unwrap_or_else(|_| format!("{:?}", args))
                    );
                }
            } else {
                debug!(
                    "🧾 WebSocket MCP 工具调用原始参数: id={:?}, params={}",
                    message.id,
                    serde_json::to_string_pretty(params).unwrap_or_else(|_| format!("{:?}", params))
                );
            }
        }

        let mut stream_guard = self.ws_stream.lock().await;
        let stream = stream_guard.as_mut().ok_or(McpError::NotInitialized)?;

        stream.send(tokio_tungstenite::tungstenite::Message::Text(json.into())).await?;
        Ok(())
    }

    /// 启动消息处理任务
    async fn start_message_handler(&self) {
        let ws_stream = self.ws_stream.clone();
        let pending_requests = self.pending_requests.clone();
        let connection_state = self.connection_state.clone();

        tokio::spawn(async move {
            loop {
                let message = {
                    let mut stream_guard = ws_stream.lock().await;
                    if let Some(stream) = stream_guard.as_mut() {
                        stream.next().await
                    } else {
                        break;
                    }
                };

                match message {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        debug!("收到MCP消息: {}", text);

                        match serde_json::from_str::<McpResponse>(&text) {
                            Ok(response) => {
                                if let serde_json::Value::String(request_id) = &response.id
                                    && let Some(sender) = pending_requests.write().await.remove(request_id)
                                {
                                    let _ = sender.send(response);
                                }
                            },
                            Err(e) => {
                                error!("解析MCP响应失败: {}", e);
                            },
                        }
                    },
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                        info!("MCP连接已关闭");
                        *connection_state.write().await = McpConnectionState::Disconnected;
                        break;
                    },
                    Some(Err(e)) => {
                        error!("MCP连接错误: {}", e);
                        *connection_state.write().await = McpConnectionState::Error(e.to_string());
                        break;
                    },
                    None => {
                        info!("MCP连接结束");
                        break;
                    },
                    _ => {
                        // 忽略其他消息类型
                    },
                }
            }

            // 清理待处理请求
            let mut pending = pending_requests.write().await;
            let senders: Vec<_> = pending.drain().collect();
            drop(pending); // 释放锁

            for (_, sender) in senders {
                let _ = sender.send(McpResponse {
                    jsonrpc: JSONRPC_VERSION.to_string(),
                    id: serde_json::Value::Null,
                    result: None,
                    error: Some(crate::mcp::protocol::McpError { code: -1, message: "连接中断".to_string(), data: None }),
                });
            }
        });
    }

    /// 生成请求ID
    async fn generate_request_id(&self) -> String {
        let mut counter = self.request_id_counter.lock().await;
        *counter += 1;
        format!("req_{}", *counter)
    }

    /// 获取服务器信息
    pub async fn get_server_info(&self) -> Option<ServerInfo> {
        self.server_info.read().await.clone()
    }

    /// 清除工具缓存
    pub async fn clear_tools_cache(&self) {
        let mut cache = self.tools_cache.write().await;
        cache.0.clear();
    }
}
