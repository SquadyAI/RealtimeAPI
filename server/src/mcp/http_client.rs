use anyhow::Result;
use chrono;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, error, info}; // 用于生成时间戳

/// HTTP MCP 客户端配置
#[derive(Debug, Clone)]
pub struct HttpMcpConfig {
    pub endpoint: String,
    pub timeout_secs: u64,
    /// Optional JWT authorization token to include in requests
    pub authorization: Option<String>,
}

/// 服务器返回的工具格式（嵌套结构）
#[derive(Debug, Clone, Deserialize)]
pub struct ServerToolResponse {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ServerFunctionInfo,
}

/// 服务器返回的function信息
#[derive(Debug, Clone, Deserialize)]
pub struct ServerFunctionInfo {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(rename = "updatedAt", default)]
    pub updated_at: Option<String>,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(default)]
    pub examples: Option<serde_json::Value>,
    #[serde(rename = "responseSchema", default)]
    pub response_schema: Option<serde_json::Value>,
}

/// 将服务器工具格式转换为内部McpTool格式
impl From<ServerToolResponse> for crate::mcp::McpTool {
    fn from(server_tool: ServerToolResponse) -> Self {
        crate::mcp::McpTool {
            name: server_tool.function.name,
            description: server_tool.function.description,
            input_schema: server_tool.function.parameters,
        }
    }
}

/// 🔧 自定义HTTP MCP工具调用的function_call结构（非标准MCP协议）
#[derive(Debug, Clone, Serialize)]
pub struct CustomHttpFunctionCall {
    pub name: String,
    pub arguments: String,
    pub ts: String, // 时间戳
}

/// 🔧 自定义HTTP MCP工具调用请求体（非标准MCP协议）
/// 用户使用自定义格式，包含utterance字段和简化的function_call
#[derive(Debug, Clone, Serialize)]
pub struct McpHttpRequest {
    pub utterance: String, // 用户原始输入
    pub session_id: String,
    pub function_call: CustomHttpFunctionCall,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_ip: Option<String>, // 用户IP地址
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_city: Option<String>, // 用户城市信息
}

/// MCP 工具列表响应
#[derive(Debug, Clone, Deserialize)]
pub struct McpToolsListResponse {
    pub tools: Vec<crate::mcp::McpTool>,
    #[serde(default)]
    pub cache_ttl_secs: Option<u64>, // 服务器建议的缓存TTL（秒）
}

/// MCP 服务器响应的控制字段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpControlResponse {
    pub mode: String, // "llm", "tts", "stop"
}

/// MCP HTTP 服务器完整响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpResponse {
    pub control: McpControlResponse,
    pub payload: serde_json::Value,
}

/// HTTP MCP 客户端
#[derive(Debug)]
pub struct HttpMcpClient {
    config: HttpMcpConfig,
    http_client: reqwest::Client,
}

impl HttpMcpClient {
    /// 创建新的 HTTP MCP 客户端
    pub fn new(config: HttpMcpConfig) -> Self {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .pool_idle_timeout(Duration::from_secs(30)) // 保持连接池30秒
            .pool_max_idle_per_host(5); // 每个主机最多保持5个空闲连接

        // 添加默认Authorization头（如果提供）
        if let Some(token) = &config.authorization {
            use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
            let mut headers = HeaderMap::new();
            let value = format!("Bearer {}", token);
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&value).expect("Invalid token"));
            builder = builder.default_headers(headers);
        }

        let http_client = builder.build().expect("Failed to create HTTP client");

        Self { config, http_client }
    }

    /// 获取工具列表（HTTP MCP 方式）
    pub async fn get_tools(&self) -> Result<McpToolsListResponse> {
        info!("🔧 发送HTTP MCP工具列表请求: endpoint={}", self.config.endpoint);

        // 构造工具列表请求URL - 使用GET请求
        let url = reqwest::Url::parse(&self.config.endpoint).map_err(|e| anyhow::anyhow!("解析工具URL失败: {}", e))?;
        info!("🔧 GET请求URL: {}", url);
        info!(
            "📤 HTTP 请求详情: method=GET, has_authorization={}, timeout_secs={}",
            self.config.authorization.is_some(),
            self.config.timeout_secs
        );

        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HTTP MCP 工具列表请求失败: {}", e))?;

        let status = response.status();
        let headers = response.headers().clone();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            error!(
                "❌ HTTP MCP 工具列表请求失败: status={} ({:?})",
                status.as_u16(),
                status.canonical_reason()
            );
            debug!("🧾 HTTP 响应头: {:?}", headers);
            // 尝试格式化JSON错误体
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                error!(
                    "❌ HTTP MCP 错误响应体(JSON): {}",
                    serde_json::to_string_pretty(&json).unwrap_or_else(|_| text.clone())
                );
            } else {
                error!("❌ HTTP MCP 错误响应体(文本): {}", text);
            }
            return Err(anyhow::anyhow!("HTTP MCP 工具列表请求失败: {} - {}", status, text));
        }

        debug!("🧾 HTTP 响应头(成功): {:?}", headers);

        // 获取响应文本用于调试和多种格式解析
        let response_text = response.text().await.map_err(|e| anyhow::anyhow!("读取响应文本失败: {}", e))?;

        // debug!("服务器原始响应: {}", response_text);

        // 尝试解析为包装格式 {tools: [...], cache_ttl_secs: ...}
        if let Ok(tools_response) = serde_json::from_str::<McpToolsListResponse>(&response_text) {
            info!("✅ 解析为包装格式: {} 个工具", tools_response.tools.len());
            return Ok(tools_response);
        }

        // 尝试解析为工具数组格式 [{type: "function", function: {...}}, ...]
        if let Ok(server_tools) = serde_json::from_str::<Vec<ServerToolResponse>>(&response_text) {
            let tools: Vec<crate::mcp::McpTool> = server_tools.into_iter().map(|server_tool| server_tool.into()).collect();

            let tools_response = McpToolsListResponse { tools, cache_ttl_secs: None };

            info!("✅ 解析为工具数组格式: {} 个工具", tools_response.tools.len());
            // debug!("HTTP MCP工具详情: {:?}", tools_response.tools);
            return Ok(tools_response);
        }

        // 3) 兼容更多服务端响应包装格式
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&response_text) {
            // 提取 TTL（可选，多个位置兼容）
            let ttl = v
                .get("cache_ttl_secs")
                .and_then(|x| x.as_u64())
                .or_else(|| v.get("data").and_then(|d| d.get("cache_ttl_secs")).and_then(|x| x.as_u64()));

            // helpers
            let try_convert_tools = |tools_value: &serde_json::Value| -> Option<Vec<crate::mcp::McpTool>> {
                // 优先按 ServerToolResponse 解析
                if let Ok(server_tools) = serde_json::from_value::<Vec<ServerToolResponse>>(tools_value.clone()) {
                    let tools: Vec<crate::mcp::McpTool> = server_tools.into_iter().map(|t| t.into()).collect();
                    return Some(tools);
                }
                // 其次直接按内部 McpTool 解析（若服务器已是内部格式）
                if let Ok(tools) = serde_json::from_value::<Vec<crate::mcp::McpTool>>(tools_value.clone()) {
                    return Some(tools);
                }
                None
            };

            // 尝试 paths: tools / data.tools / result.tools / payload.tools / data
            let candidates: Vec<&serde_json::Value> = vec![
                v.get("tools").unwrap_or(&serde_json::Value::Null),
                v.get("data").and_then(|d| d.get("tools")).unwrap_or(&serde_json::Value::Null),
                v.get("result").and_then(|d| d.get("tools")).unwrap_or(&serde_json::Value::Null),
                v.get("payload")
                    .and_then(|d| d.get("tools"))
                    .unwrap_or(&serde_json::Value::Null),
                v.get("data").unwrap_or(&serde_json::Value::Null), // 有些服务可能直接把数组放 data 下
            ];

            for candidate in candidates {
                if candidate.is_array()
                    && let Some(tools) = try_convert_tools(candidate)
                {
                    let tools_response = McpToolsListResponse { tools, cache_ttl_secs: ttl };
                    info!("✅ 解析为兼容包装格式: {} 个工具", tools_response.tools.len());
                    return Ok(tools_response);
                }
            }
        }

        // 都失败了，返回原始错误并携带响应前缀便于排查
        let preview: String = response_text.chars().take(400).collect();
        Err(anyhow::anyhow!(
            "解析HTTP MCP工具列表响应失败: 无法识别的响应格式，body_prefix={}...",
            preview
        ))
    }

    /// 调用工具（带自定义超时）
    pub async fn call_tool_with_timeout(
        &self,
        session_id: &str,
        utterance: &str,
        tool_call: &crate::llm::llm::ToolCall,
        timeout: Duration,
        user_ip: Option<String>,
        user_city: Option<String>,
    ) -> Result<McpHttpResponse> {
        // 🔧 添加详细的入参日志记录
        let tool_name = tool_call.function.name.as_deref().unwrap_or("unknown");
        let tool_id = tool_call.id.as_deref().unwrap_or("unknown");

        info!(
            "📡 调用 HTTP MCP 工具 (超时: {:?}): session_id={}, tool_id={}, tool_name={}, utterance='{}', endpoint={}",
            timeout, session_id, tool_id, tool_name, utterance, self.config.endpoint
        );
        debug!(
            "📝 HTTP MCP 工具调用参数: session_id={}, utterance='{}', tool_call={:?}",
            session_id, utterance, tool_call
        );

        // 🔍 打印MCP请求内容，包含IP和城市信息
        info!(
            "📤 HTTP MCP 请求内容: session_id={}, tool_name={}, user_ip={:?}, user_city={:?}",
            session_id, tool_name, user_ip, user_city
        );

        let request_body = McpHttpRequest {
            utterance: utterance.to_string(), // 🔧 使用真实的用户输入
            session_id: session_id.to_string(),
            function_call: CustomHttpFunctionCall {
                name: tool_name.to_string(),
                arguments: tool_call.function.arguments.clone().unwrap_or_else(|| "{}".to_string()),
                ts: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            },
            user_ip,   // 🆕 添加用户IP地址
            user_city, // 🆕 添加用户城市信息
        };

        if let Ok(pretty_request) = serde_json::to_string_pretty(&request_body) {
            info!("📤 HTTP MCP 完整请求体: {}", pretty_request);
        }

        // 🔧 使用自定义超时创建临时客户端
        let mut temp_builder = reqwest::Client::builder()
            .timeout(timeout)
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(5);
        // 添加默认Authorization头（如果提供）
        if let Some(token) = &self.config.authorization {
            use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
            let mut headers = HeaderMap::new();
            let value = format!("Bearer {}", token);
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&value).expect("Invalid token"));
            temp_builder = temp_builder.default_headers(headers);
        }
        let temp_client = temp_builder.build().expect("Failed to create temporary HTTP client");

        info!(
            "📤 发送 HTTP MCP 工具调用请求: method=POST, endpoint={}, timeout={:?}, has_authorization={}",
            self.config.endpoint,
            timeout,
            self.config.authorization.is_some()
        );
        let response = temp_client
            .post(&self.config.endpoint)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("HTTP MCP 请求失败: {}", e))?;

        let status = response.status();
        let headers = response.headers().clone();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            error!(
                "❌ HTTP MCP 工具调用失败: session_id={}, tool_name={}, status={} ({:?})",
                session_id,
                tool_name,
                status.as_u16(),
                status.canonical_reason()
            );
            debug!("🧾 HTTP 响应头: {:?}", headers);
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                error!(
                    "❌ HTTP MCP 错误响应体(JSON): {}",
                    serde_json::to_string_pretty(&json).unwrap_or_else(|_| text.clone())
                );
            } else {
                error!("❌ HTTP MCP 错误响应体(文本): {}", text);
            }
            return Err(anyhow::anyhow!("HTTP MCP 请求失败: {} - {}", status, text));
        }

        debug!("🧾 HTTP 响应头(成功): {:?}", headers);

        let mcp_response: McpHttpResponse = response.json().await.map_err(|e| anyhow::anyhow!("解析 MCP 响应失败: {}", e))?;

        // 🔍 打印MCP工具调用的原始结果
        info!(
            "📋 HTTP MCP 工具调用原始结果 - session_id: {}, tool_name: {}",
            session_id, tool_name
        );
        if let Ok(pretty_payload) = serde_json::to_string_pretty(&mcp_response.payload) {
            info!("📋 HTTP MCP 原始响应 Payload: {}", pretty_payload);
        } else {
            info!("📋 HTTP MCP 原始响应 Payload: {:?}", mcp_response.payload);
        }

        info!(
            "✅ HTTP MCP 工具调用完成: session_id={}, tool_name={}, control_mode={}",
            session_id, tool_name, mcp_response.control.mode
        );
        debug!(
            "📝 HTTP MCP 工具调用结果: session_id={}, tool_name={}, response={:?}",
            session_id, tool_name, mcp_response
        );

        Ok(mcp_response)
    }

    /// 调用工具
    pub async fn call_tool(
        &self,
        session_id: &str,
        utterance: &str, // 🆕 用户原始输入
        tool_call: &crate::llm::llm::ToolCall,
        user_ip: Option<String>,
        user_city: Option<String>,
    ) -> Result<McpHttpResponse> {
        // 🔧 使用更短的超时时间，专门针对工具调用场景
        self.call_tool_with_timeout(session_id, utterance, tool_call, Duration::from_secs(2), user_ip, user_city)
            .await
    }

    /// 获取客户端配置的URL
    pub fn get_url(&self) -> &str {
        &self.config.endpoint
    }
}
