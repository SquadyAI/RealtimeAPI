use anyhow::Result;
use rustc_hash::FxHashMap;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, error, info, warn};

use crate::llm::llm::Tool;
use crate::mcp::get_global_tools_endpoint_client;
use std::collections::HashSet;

/// 工具来源类型
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSourceType {
    /// 内置工具
    Builtin,
    /// HTTP MCP 工具，附带端点 URL
    HttpMcp(String),
    /// WebSocket MCP 工具，附带端点 URL
    WsMcp(String),
    /// tools_endpoint 加载的工具，附带端点 URL
    ToolsEndpoint(String),
    /// prompt_endpoint 返回的工具
    PromptEndpoint,
    /// 客户端直接提供的工具
    Client,
}

impl ToolSourceType {
    pub fn label(&self) -> &'static str {
        match self {
            ToolSourceType::Builtin => "builtin",
            ToolSourceType::HttpMcp(_) => "http_mcp",
            ToolSourceType::WsMcp(_) => "ws_mcp",
            ToolSourceType::ToolsEndpoint(_) => "tools_endpoint",
            ToolSourceType::PromptEndpoint => "prompt_endpoint",
            ToolSourceType::Client => "client",
        }
    }
}

/// 异步工具管理器
/// 负责在后台异步加载工具配置，并在加载完成后通知相关组件
pub struct AsyncToolsManager {
    /// 正在加载的工具端点
    loading_endpoints: Arc<RwLock<FxHashMap<String, bool>>>,
    /// 工具加载完成的广播通道（支持多订阅者）
    tools_loaded_tx: broadcast::Sender<ToolsLoadedEvent>,
    /// 端点 -> 订阅该端点的 session_id 集合（用于主动刷新后广播到所有会话）
    endpoint_sessions: Arc<RwLock<FxHashMap<String, HashSet<String>>>>,
    /// 端点 -> 刷新任务是否已启动
    endpoint_refresh_started: Arc<RwLock<FxHashMap<String, bool>>>,
}

// 会话级别的已加载工具缓存（供 LLM 轮次前注入使用）
lazy_static::lazy_static! {
    static ref SESSION_TOOLS: RwLock<FxHashMap<String, Vec<Tool>>> = RwLock::new(FxHashMap::default());
    // 工具来源注册表: session_id -> (tool_name -> source)
    pub static ref SESSION_TOOL_SOURCES: RwLock<FxHashMap<String, FxHashMap<String, ToolSourceType>>> = RwLock::new(FxHashMap::default());
}

/// 写入会话工具（覆盖）
pub async fn set_session_loaded_tools(session_id: &str, tools: Vec<Tool>) {
    let mut guard = SESSION_TOOLS.write().await;
    guard.insert(session_id.to_string(), tools);
}

/// 合并会话工具（去重，新工具优先）
pub async fn merge_session_tools(session_id: &str, new_tools: Vec<Tool>) -> usize {
    use std::collections::HashSet;
    let mut guard = SESSION_TOOLS.write().await;
    let entry = guard.entry(session_id.to_string()).or_insert_with(Vec::new);

    // 收集现有工具名称
    let existing_names: HashSet<String> = entry.iter().map(|t| t.function.name.clone()).collect();

    // 只添加不存在的新工具
    let mut added = 0;
    for tool in new_tools {
        if !existing_names.contains(&tool.function.name) {
            entry.push(tool);
            added += 1;
        }
    }
    added
}

/// 读取会话工具（克隆，不清除）
pub async fn get_session_loaded_tools(session_id: &str) -> Vec<Tool> {
    let guard = SESSION_TOOLS.read().await;
    guard.get(session_id).cloned().unwrap_or_default()
}

/// 注册工具来源（批量）
pub async fn register_tool_sources(session_id: &str, tools: &[Tool], source: ToolSourceType) {
    let mut guard = SESSION_TOOL_SOURCES.write().await;
    let entry = guard.entry(session_id.to_string()).or_insert_with(FxHashMap::default);
    for tool in tools {
        entry.insert(tool.function.name.clone(), source.clone());
    }
    debug!(
        "📝 注册工具来源: session={}, count={}, source={:?}",
        session_id,
        tools.len(),
        source
    );
}

/// 查询工具来源
pub async fn get_tool_source(session_id: &str, tool_name: &str) -> Option<ToolSourceType> {
    let guard = SESSION_TOOL_SOURCES.read().await;
    guard.get(session_id).and_then(|m| m.get(tool_name).cloned())
}

/// 清除会话的所有工具数据（工具列表 + 来源注册）
pub async fn clear_session_tools(session_id: &str) {
    // 清理工具列表
    {
        let mut guard = SESSION_TOOLS.write().await;
        if guard.remove(session_id).is_some() {
            debug!("🧹 已清理会话工具列表: session={}", session_id);
        }
    }
    // 清理来源注册
    {
        let mut guard = SESSION_TOOL_SOURCES.write().await;
        if guard.remove(session_id).is_some() {
            debug!("🧹 已清理会话工具来源注册: session={}", session_id);
        }
    }
}

/// 工具加载完成事件
#[derive(Debug, Clone)]
pub struct ToolsLoadedEvent {
    pub endpoint: String,
    pub session_id: String,
    pub tools: Vec<Tool>,
    pub success: bool,
    pub error: Option<String>,
}

impl Default for AsyncToolsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncToolsManager {
    /// 创建新的异步工具管理器
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(128);

        Self {
            loading_endpoints: Arc::new(RwLock::new(FxHashMap::default())),
            tools_loaded_tx: tx,
            endpoint_sessions: Arc::new(RwLock::new(FxHashMap::default())),
            endpoint_refresh_started: Arc::new(RwLock::new(FxHashMap::default())),
        }
    }

    /// 启动异步工具加载
    pub async fn start_async_tools_loading(&self, endpoint: String, session_id: String) -> Result<()> {
        // 记录订阅关系
        {
            let mut map = self.endpoint_sessions.write().await;
            map.entry(endpoint.clone())
                .or_insert_with(HashSet::new)
                .insert(session_id.clone());
        }

        // 检查是否已经在加载
        {
            let mut loading = self.loading_endpoints.write().await;
            if loading.get(&endpoint).copied().unwrap_or(false) {
                info!("🔧 工具端点已在加载中: {}", endpoint);
            } else {
                loading.insert(endpoint.clone(), true);
            }
        }

        info!("🚀 启动异步工具加载: {} (session: {})", endpoint, session_id);

        let loading_endpoints = self.loading_endpoints.clone();
        let tools_loaded_tx = self.tools_loaded_tx.clone();
        let endpoint_sessions = self.endpoint_sessions.clone();

        // 启动异步任务
        let endpoint_for_spawn = endpoint.clone();
        tokio::spawn(async move {
            let result = Self::load_tools_from_endpoint(&endpoint_for_spawn).await;

            // 标记加载完成
            {
                let mut loading = loading_endpoints.write().await;
                loading.remove(&endpoint_for_spawn);
            }

            // 发送加载完成事件
            let event = match result {
                Ok(tools) => {
                    info!("✅ 异步工具加载成功: {} ({} 个工具)", endpoint_for_spawn, tools.len());
                    // 将工具合并到会话级缓存（不覆盖已有工具，避免时序竞争）
                    let added = merge_session_tools(&session_id, tools.clone()).await;
                    info!("🔧 tools_endpoint 工具已合并: 新增 {} 个", added);
                    // 注册工具来源
                    register_tool_sources(&session_id, &tools, ToolSourceType::ToolsEndpoint(endpoint_for_spawn.clone())).await;
                    ToolsLoadedEvent {
                        endpoint: endpoint_for_spawn.clone(),
                        session_id: session_id.clone(),
                        tools,
                        success: true,
                        error: None,
                    }
                },
                Err(e) => {
                    error!("❌ 异步工具加载失败: {} - {}", endpoint_for_spawn, e);
                    ToolsLoadedEvent {
                        endpoint: endpoint_for_spawn.clone(),
                        session_id: session_id.clone(),
                        tools: Vec::new(),
                        success: false,
                        error: Some(e.to_string()),
                    }
                },
            };

            if let Err(e) = tools_loaded_tx.send(event) {
                warn!("⚠️ 发送工具加载完成事件失败: {}", e);
            }

            // 🆕 确保刷新任务已启动：按 TTL 定期主动刷新该端点
            let _ = endpoint_sessions.write().await; // 仅触发一次写锁，随后释放
        });

        // 启动端点刷新任务（仅启动一次）
        {
            let mut started = self.endpoint_refresh_started.write().await;
            if !started.get(&endpoint).copied().unwrap_or(false) {
                started.insert(endpoint.clone(), true);

                let endpoint_clone = endpoint.clone();
                let endpoint_sessions = self.endpoint_sessions.clone();
                let tools_loaded_tx = self.tools_loaded_tx.clone();

                tokio::spawn(async move {
                    // 使用 ToolsEndpointClient 的默认 TTL 作为刷新周期（没有服务端 TTL）
                    const REFRESH_INTERVAL_SECS: u64 = 300; // 5分钟
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(REFRESH_INTERVAL_SECS));
                    loop {
                        interval.tick().await;
                        // 刷新该端点的工具
                        match Self::load_tools_from_endpoint(&endpoint_clone).await {
                            Ok(tools) => {
                                // 对所有订阅此端点的会话广播
                                let sessions: Vec<String> = {
                                    let map = endpoint_sessions.read().await;
                                    map.get(&endpoint_clone)
                                        .map(|s| s.iter().cloned().collect())
                                        .unwrap_or_default()
                                };
                                let broadcast_count = sessions.len();
                                for sid in sessions {
                                    // 合并工具（不覆盖已有工具）
                                    merge_session_tools(&sid, tools.clone()).await;
                                    // 注册工具来源
                                    register_tool_sources(&sid, &tools, ToolSourceType::ToolsEndpoint(endpoint_clone.clone())).await;
                                    let evt = ToolsLoadedEvent {
                                        endpoint: endpoint_clone.clone(),
                                        session_id: sid,
                                        tools: tools.clone(),
                                        success: true,
                                        error: None,
                                    };
                                    let _ = tools_loaded_tx.send(evt);
                                }
                                info!("🔁 工具端点已主动刷新: {}, 会话广播数: {}", endpoint_clone, broadcast_count);
                            },
                            Err(e) => {
                                warn!("⚠️ 工具端点主动刷新失败: {} - {}", endpoint_clone, e);
                            },
                        }
                    }
                });
            }
        }

        Ok(())
    }

    /// 从工具端点加载工具
    async fn load_tools_from_endpoint(endpoint: &str) -> Result<Vec<Tool>> {
        info!("🔧 开始从工具端点加载工具: {}", endpoint);

        // 从工具端点获取工具列表
        let tools_values = get_global_tools_endpoint_client()
            .get_tools(endpoint)
            .await
            .map_err(|e| anyhow::anyhow!("获取工具端点失败: {}", e))?;

        // 转换为LLM工具格式
        let mut llm_tools = Vec::new();
        for tool_value in tools_values {
            match Self::convert_tool_value_to_llm_tool(tool_value) {
                Ok(mut llm_tool) => {
                    // 修正非标准 JSON Schema 类型（如 "int" → "integer"）
                    Self::fix_schema_types(&mut llm_tool.function.parameters);
                    // 对 get_weather 工具的 language 参数进行扩展，添加更多语言支持
                    Self::patch_weather_tool_languages(&mut llm_tool);
                    llm_tools.push(llm_tool);
                },
                Err(e) => {
                    warn!("⚠️ 跳过无效工具: {}", e);
                },
            }
        }

        info!("✅ 成功转换 {} 个工具", llm_tools.len());
        Ok(llm_tools)
    }

    /// 递归修正 JSON Schema 中的非标准类型
    /// 例如 MCP 服务端可能返回 "int" 而非 "integer"，"bool" 而非 "boolean"
    pub fn fix_schema_types(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(type_val) = map.get_mut("type") {
                    if let Some(t) = type_val.as_str() {
                        let fixed = match t {
                            "int" => Some("integer"),
                            "float" | "double" => Some("number"),
                            "bool" => Some("boolean"),
                            "str" => Some("string"),
                            _ => None,
                        };
                        if let Some(correct) = fixed {
                            debug!("🔧 修正 schema 类型: '{}' → '{}'", t, correct);
                            *type_val = serde_json::Value::String(correct.to_string());
                        }
                    }
                }
                for (_, v) in map.iter_mut() {
                    Self::fix_schema_types(v);
                }
            },
            serde_json::Value::Array(arr) => {
                for v in arr.iter_mut() {
                    Self::fix_schema_types(v);
                }
            },
            _ => {},
        }
    }

    /// 扩展 get_weather 工具的 language 参数，添加更多语言支持
    fn patch_weather_tool_languages(tool: &mut Tool) {
        if tool.function.name != "get_weather" {
            return;
        }

        if let Some(props) = tool.function.parameters.get_mut("properties") {
            if let Some(lang) = props.get_mut("language") {
                if let Some(enum_arr) = lang.get_mut("enum") {
                    if let Some(arr) = enum_arr.as_array_mut() {
                        // 添加西班牙语和意大利语支持
                        let es = serde_json::Value::String("es".to_string());
                        let it = serde_json::Value::String("it".to_string());
                        if !arr.contains(&es) {
                            arr.push(es);
                        }
                        if !arr.contains(&it) {
                            arr.push(it);
                        }
                        debug!("🌐 已扩展 get_weather 工具的 language 参数: {:?}", arr);
                    }
                }
            }
        }
    }

    /// 将工具值转换为LLM工具格式
    fn convert_tool_value_to_llm_tool(tool_value: Value) -> Result<Tool, String> {
        // 尝试直接解析为标准工具格式
        if let Ok(tool) = serde_json::from_value::<Tool>(tool_value.clone()) {
            return Ok(tool);
        }

        // 尝试解析为MCP工具格式并转换
        if let Ok(mcp_tool) = serde_json::from_value::<crate::mcp::McpTool>(tool_value.clone()) {
            let llm_tool: Tool = mcp_tool.into();
            return Ok(llm_tool);
        }

        // 尝试解析为嵌套格式 {type: "function", function: {...}}
        if let Some(tool_obj) = tool_value.as_object()
            && let (Some(tool_type), Some(function_obj)) = (tool_obj.get("type"), tool_obj.get("function"))
            && tool_type == "function"
            && function_obj.is_object()
            && let Ok(function) = serde_json::from_value::<crate::llm::llm::ToolFunction>(function_obj.clone())
        {
            return Ok(Tool { tool_type: "function".to_string(), function });
        }

        Err(format!("无法解析工具格式: {}", tool_value))
    }

    /// 检查工具端点是否正在加载
    pub async fn is_loading(&self, endpoint: &str) -> bool {
        let loading = self.loading_endpoints.read().await;
        loading.get(endpoint).copied().unwrap_or(false)
    }

    /// 订阅工具加载完成事件（广播）
    pub fn subscribe(&self) -> broadcast::Receiver<ToolsLoadedEvent> {
        self.tools_loaded_tx.subscribe()
    }
}

// 全局异步工具管理器
lazy_static::lazy_static! {
    static ref GLOBAL_ASYNC_TOOLS_MANAGER: Arc<AsyncToolsManager> = {
        let manager = AsyncToolsManager::new();
        Arc::new(manager)
    };
}

/// 获取全局异步工具管理器
pub fn get_global_async_tools_manager() -> Arc<AsyncToolsManager> {
    GLOBAL_ASYNC_TOOLS_MANAGER.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_async_tools_manager_creation() {
        let manager = AsyncToolsManager::new();
        assert!(!manager.is_loading("test_endpoint").await);
    }

    #[tokio::test]
    async fn test_tool_conversion() {
        let tool_value = serde_json::json!({
            "type": "function",
            "function": {
                "name": "test_tool",
                "description": "测试工具",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "param1": {"type": "string"}
                    }
                }
            }
        });

        let result = AsyncToolsManager::convert_tool_value_to_llm_tool(tool_value);
        assert!(result.is_ok());

        let tool = result.unwrap();
        assert_eq!(tool.tool_type, "function");
        assert_eq!(tool.function.name, "test_tool");
    }
}
