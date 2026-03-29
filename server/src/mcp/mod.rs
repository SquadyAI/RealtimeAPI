use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::mem;

pub mod async_tools_manager;

/// 自定义反序列化器：接受整数或浮点数并转换为u64
fn deserialize_u64_from_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u)
            } else if let Some(f) = n.as_f64() {
                Ok(f as u64)
            } else {
                Err(serde::de::Error::custom("无法将数字转换为u64"))
            }
        },
        _ => Err(serde::de::Error::custom("期望数字类型")),
    }
}

/// 自定义反序列化器：接受整数或浮点数并转换为u32
fn deserialize_u32_from_number<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(u as u32)
            } else if let Some(f) = n.as_f64() {
                Ok(f as u32)
            } else {
                Err(serde::de::Error::custom("无法将数字转换为u32"))
            }
        },
        _ => Err(serde::de::Error::custom("期望数字类型")),
    }
}
pub mod client;
pub mod error;
pub mod http_client;
pub mod manager;
pub mod protocol;
pub mod tool_cache;
pub mod tools_endpoint_client;

pub use async_tools_manager::{
    AsyncToolsManager, ToolSourceType, ToolsLoadedEvent, clear_session_tools, get_global_async_tools_manager, get_session_loaded_tools, get_tool_source, merge_session_tools, register_tool_sources,
    set_session_loaded_tools,
};
pub use client::McpClient;
pub use error::McpError;
pub use http_client::{HttpMcpClient, HttpMcpConfig, McpControlResponse, McpHttpResponse, McpToolsListResponse};
pub use manager::{McpClientState, McpManager};
pub use protocol::*;
pub use tool_cache::{GLOBAL_MCP_TOOL_CACHE, GlobalMcpToolCache};
pub use tools_endpoint_client::{ToolsEndpointClient, ToolsEndpointConfig, get_global_tools_endpoint_client};

/// MCP工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// MCP工具调用请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// MCP工具调用结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolResult {
    pub content: Vec<McpContent>,
    pub is_error: bool,
}

/// MCP内容类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "resource")]
    Resource { resource: McpResourceReference },
}

/// MCP资源引用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResourceReference {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// MCP服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// MCP服务器URL (支持 ws://, wss://, http://, https:// 或 stdio://)
    #[serde(alias = "url", alias = "endpoint")]
    pub endpoint: String,
    /// 可选的JWT授权令牌，将在请求时作为Authorization头发送
    #[serde(default)]
    pub authorization: Option<String>,

    /// 连接超时时间（秒）
    #[serde(default = "default_timeout_secs", deserialize_with = "deserialize_u64_from_number")]
    pub timeout_secs: u64,
    /// 重连间隔（秒）
    #[serde(default = "default_reconnect_interval_secs", deserialize_with = "deserialize_u64_from_number")]
    pub reconnect_interval_secs: u64,
    /// 最大重连次数
    #[serde(default = "default_max_reconnect_attempts", deserialize_with = "deserialize_u32_from_number")]
    pub max_reconnect_attempts: u32,
    /// 工具缓存TTL（秒）
    #[serde(default = "default_tool_cache_ttl_secs", deserialize_with = "deserialize_u64_from_number")]
    pub tool_cache_ttl_secs: u64,
}

fn default_timeout_secs() -> u64 {
    10
}
fn default_reconnect_interval_secs() -> u64 {
    5
}
fn default_max_reconnect_attempts() -> u32 {
    3
}
fn default_tool_cache_ttl_secs() -> u64 {
    300
}

impl McpServerConfig {
    /// 检测是否为HTTP协议的MCP服务器
    pub fn is_http_protocol(&self) -> bool {
        self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://")
    }

    /// 检测是否为WebSocket协议的MCP服务器
    pub fn is_websocket_protocol(&self) -> bool {
        self.endpoint.starts_with("ws://") || self.endpoint.starts_with("wss://")
    }

    /// 检测是否为stdio协议的MCP服务器
    pub fn is_stdio_protocol(&self) -> bool {
        self.endpoint.starts_with("stdio://")
    }

    /// 转换为HttpMcpConfig（仅当为HTTP协议时有效）
    pub fn to_http_config(&self) -> Option<HttpMcpConfig> {
        if self.is_http_protocol() {
            Some(HttpMcpConfig {
                endpoint: self.endpoint.clone(),
                timeout_secs: self.timeout_secs,
                authorization: self.authorization.clone(),
            })
        } else {
            None
        }
    }
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            authorization: None,
            timeout_secs: default_timeout_secs(),
            reconnect_interval_secs: default_reconnect_interval_secs(),
            max_reconnect_attempts: default_max_reconnect_attempts(),
            tool_cache_ttl_secs: default_tool_cache_ttl_secs(),
        }
    }
}

/// MCP连接状态
#[derive(Debug, Clone, PartialEq)]
pub enum McpConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// 递归保留 OpenAI 支持的 JSON-Schema 字段，过滤掉 oneOf / const / format 等。
fn sanitize_json_schema(schema: Value) -> Value {
    // OpenAI 当前官方文档中的白名单字段
    const ALLOWED_KEYS: [&str; 7] = [
        "type",
        "description",
        "properties",
        "required",
        "enum",
        "items", // 支持array类型
        "additionalProperties",
    ];

    match schema {
        Value::Object(mut map) => {
            // 递归处理 `properties` 字段中的每个子schema
            if let Some(Value::Object(properties)) = map.get_mut("properties") {
                for prop_schema in properties.values_mut() {
                    let taken = mem::take(prop_schema);
                    *prop_schema = sanitize_json_schema(taken);
                }
            }

            // 递归处理 `items` 字段 (用于array类型)
            if let Some(items_schema) = map.get_mut("items") {
                let taken = mem::take(items_schema);
                *items_schema = sanitize_json_schema(taken);
            }

            // 只在当前层级保留白名单中的键
            map.retain(|k, _| ALLOWED_KEYS.contains(&k.as_str()));
            Value::Object(map)
        },
        Value::Array(arr) => {
            // 通常是 "required" 或 "enum" 的值列表，不需要处理
            Value::Array(arr)
        },
        other => other,
    }
}

/// 将MCP工具转换为LLM工具格式
impl From<McpTool> for crate::llm::llm::Tool {
    fn from(mcp_tool: McpTool) -> Self {
        let mut parameters = sanitize_json_schema(mcp_tool.input_schema);
        // 修正非标准 JSON Schema 类型（如 MCP 服务端返回 "int" 而非 "integer"）
        crate::mcp::async_tools_manager::AsyncToolsManager::fix_schema_types(&mut parameters);
        crate::llm::llm::Tool {
            tool_type: "function".to_string(),
            function: crate::llm::llm::ToolFunction { name: mcp_tool.name, description: mcp_tool.description, parameters },
        }
    }
}

/// 将LLM工具调用转换为MCP工具调用
impl TryFrom<&crate::llm::llm::ToolCall> for McpToolCall {
    type Error = McpError;

    fn try_from(tool_call: &crate::llm::llm::ToolCall) -> Result<Self, Self::Error> {
        let arguments: serde_json::Value = if let Some(args) = &tool_call.function.arguments {
            serde_json::from_str(args).map_err(|e| McpError::InvalidArguments(format!("Failed to parse arguments: {}", e)))?
        } else {
            serde_json::json!({})
        };

        Ok(McpToolCall { name: tool_call.function.name.clone().unwrap_or_default(), arguments })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_config_minimal_fields() {
        // 测试只有 endpoint 字段的配置能否成功解析
        let json = r#"{"endpoint": "http://localhost:8080/mcp"}"#;
        let config: McpServerConfig = serde_json::from_str(json).expect("解析失败");

        assert_eq!(config.endpoint, "http://localhost:8080/mcp");
        assert_eq!(config.timeout_secs, 10);
        assert_eq!(config.reconnect_interval_secs, 5);
        assert_eq!(config.max_reconnect_attempts, 3);
        assert_eq!(config.tool_cache_ttl_secs, 300);
    }

    #[test]
    fn test_mcp_config_array_minimal() {
        // 测试数组格式的配置解析
        let json = r#"[
            {"endpoint": "http://localhost:8080/mcp1"},
            {"endpoint": "http://localhost:8081/mcp2", "timeout_secs": 60}
        ]"#;
        let configs: Vec<McpServerConfig> = serde_json::from_str(json).expect("解析失败");

        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].endpoint, "http://localhost:8080/mcp1");
        assert_eq!(configs[0].timeout_secs, 10); // 使用默认值
        assert_eq!(configs[1].endpoint, "http://localhost:8081/mcp2");
        assert_eq!(configs[1].timeout_secs, 60); // 使用指定值
    }

    #[test]
    fn test_mcp_tool_conversion_preserves_parameters() {
        // 测试MCP工具转换是否正确保留参数信息
        let mcp_tool = McpTool {
            name: "query_weather".to_string(),
            description: "用户询问天气时调用".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "查询的地点，如\"上海市\"。"
                    },
                    "language": {
                        "type": "string",
                        "enum": ["zh", "en", "ja", "ko"],
                        "default": "zh",
                        "description": "回复使用的语言代码。"
                    },
                    "startTime": {
                        "type": "string",
                        "description": "查询天气的开始时间，要求：返回格式为 'yyyy-MM-dd'。"
                    }
                },
                "required": ["location"],
                "oneOf": [{"properties": {"mode": {"const": "simple"}}}],
                "default": {"language": "zh"}
            }),
        };

        // 转换为LLM工具格式
        let llm_tool: crate::llm::llm::Tool = mcp_tool.into();

        // 验证基本信息
        assert_eq!(llm_tool.tool_type, "function");
        assert_eq!(llm_tool.function.name, "query_weather");
        assert_eq!(llm_tool.function.description, "用户询问天气时调用");

        // 验证参数是否正确保留
        let params = &llm_tool.function.parameters;
        println!("转换后的参数: {}", serde_json::to_string_pretty(params).unwrap());

        // 检查properties是否存在且包含location
        assert!(params["properties"].is_object());
        assert!(params["properties"]["location"].is_object());
        assert_eq!(params["properties"]["location"]["type"], "string");
        assert_eq!(params["properties"]["location"]["description"], "查询的地点，如\"上海市\"。");

        // 检查language参数
        assert!(params["properties"]["language"].is_object());
        assert_eq!(params["properties"]["language"]["type"], "string");
        assert!(params["properties"]["language"]["enum"].is_array());

        // 检查required字段
        assert!(params["required"].is_array());
        assert_eq!(params["required"][0], "location");

        // 检查不支持的字段是否被正确移除
        assert!(params["oneOf"].is_null() || !params.as_object().unwrap().contains_key("oneOf"));
        assert!(params["default"].is_null() || !params.as_object().unwrap().contains_key("default"));

        // 验证参数不为空
        assert!(!params["properties"].as_object().unwrap().is_empty());
    }
}
