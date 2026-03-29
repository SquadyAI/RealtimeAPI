use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP JSON-RPC请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// MCP JSON-RPC响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

/// MCP JSON-RPC错误
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// MCP初始化请求参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

/// MCP客户端能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
}

/// Roots能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// 客户端信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// MCP初始化响应结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
}

/// MCP服务器能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

/// 提示能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// 资源能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourcesCapability {
    #[serde(rename = "subscribe")]
    pub subscribe: bool,
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// 工具能力
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

/// 服务器信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

/// 工具列表请求参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// 工具列表响应结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// MCP工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// 工具调用请求参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// 工具调用响应结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<Content>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

/// MCP内容
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource")]
    Resource { resource: ResourceReference },
}

/// 资源引用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceReference {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

// 常量定义
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
pub const JSONRPC_VERSION: &str = "2.0";

// 方法名常量
pub const METHOD_INITIALIZE: &str = "initialize";
pub const METHOD_INITIALIZED: &str = "initialized";
pub const METHOD_LIST_TOOLS: &str = "tools/list";
pub const METHOD_CALL_TOOL: &str = "tools/call";
pub const METHOD_LIST_RESOURCES: &str = "resources/list";
pub const METHOD_READ_RESOURCE: &str = "resources/read";
pub const METHOD_LIST_PROMPTS: &str = "prompts/list";
pub const METHOD_GET_PROMPT: &str = "prompts/get";

impl McpRequest {
    /// 创建初始化请求
    pub fn initialize(id: Value, client_name: String, client_version: String) -> Self {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities { roots: Some(RootsCapability { list_changed: true }), sampling: None },
            client_info: ClientInfo { name: client_name, version: client_version },
        };

        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: METHOD_INITIALIZE.to_string(),
            params: Some(serde_json::to_value(params).unwrap()),
        }
    }

    /// 创建工具列表请求
    pub fn list_tools(id: Value, cursor: Option<String>) -> Self {
        let params = ListToolsParams { cursor };

        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: METHOD_LIST_TOOLS.to_string(),
            params: Some(serde_json::to_value(params).unwrap()),
        }
    }

    /// 创建工具调用请求
    pub fn call_tool(id: Value, name: String, arguments: Option<Value>) -> Self {
        let params = CallToolParams { name, arguments };

        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id,
            method: METHOD_CALL_TOOL.to_string(),
            params: Some(serde_json::to_value(params).unwrap()),
        }
    }

    /// 创建initialized通知
    pub fn initialized() -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Value::Null,
            method: METHOD_INITIALIZED.to_string(),
            params: None,
        }
    }
}
