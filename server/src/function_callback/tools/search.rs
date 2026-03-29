use crate::env_utils::env_string_or_default;
use crate::function_callback::searxng_client;
use crate::function_callback::{CallResult, FunctionCallbackError};
use rustc_hash::FxHashMap;
use tracing::{info, warn};

use super::tavily_client;

/// 获取当前搜索后端类型
fn get_search_backend() -> &'static str {
    static BACKEND: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BACKEND.get_or_init(|| {
        let backend = env_string_or_default("SEARCH_BACKEND", "searxng");
        info!("🔍 搜索后端: {}", backend);
        backend
    })
}

/// 内置搜索工具管理器
pub struct BuiltinSearchManager {
    /// 🆕 默认搜索选项（可被工具参数覆盖）
    default_options: std::sync::RwLock<searxng_client::SearchOptions>,
}

impl Default for BuiltinSearchManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BuiltinSearchManager {
    pub fn new() -> Self {
        Self { default_options: std::sync::RwLock::new(Default::default()) }
    }

    /// 初始化搜索客户端 - 现在只用于配置搜索参数，不创建新客户端
    pub async fn init_search_client(&self, config: Option<serde_json::Value>) -> Result<(), FunctionCallbackError> {
        // 🔒 检查全局客户端是否已初始化
        searxng_client::get_searxng_client().await?;

        // 🔒 如果提供了配置，只记录搜索参数（不影响全局客户端）
        if let Some(user_config) = config {
            let search_config = searxng_client::SearXNGConfig::search_params_only().with_search_params(&user_config);

            info!(
                "🔍 用户搜索参数配置: 引擎={:?}, 语言={}, 安全搜索={}, 最大结果={}",
                search_config.default_engines, search_config.default_language, search_config.safe_search, search_config.max_results
            );

            // 注意：这些配置仅用于记录，实际搜索使用全局客户端的配置
            warn!("⚠️ 搜索参数配置已记录，但当前搜索引擎使用全局固定配置");
        } else {
            info!("🔍 使用默认搜索配置");
        }

        info!("✅ 使用全局SearXNG客户端（已预热）");
        Ok(())
    }

    /// 🆕 设置默认搜索选项（线程安全）
    pub fn set_default_options(&self, options: searxng_client::SearchOptions) {
        if let Ok(mut guard) = self.default_options.write() {
            *guard = options;
            tracing::info!("🔧 已更新内置搜索默认参数: {:?}", *guard);
        }
    }

    /// 🆕 获取默认搜索选项的克隆
    pub fn get_default_options(&self) -> searxng_client::SearchOptions {
        self.default_options.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// 执行搜索
    pub async fn search(&self, query: &str, options: Option<searxng_client::SearchOptions>) -> Result<CallResult, FunctionCallbackError> {
        // 🔧 直接使用全局客户端
        let client = searxng_client::get_searxng_client().await?;

        let effective_options = if let Some(opts) = options { opts } else { self.get_default_options() };
        let response = client.search(query, Some(effective_options)).await?;
        Ok(CallResult::Success(serde_json::to_value(response).map_err(|e| {
            FunctionCallbackError::Other(format!("序列化搜索结果失败: {}", e))
        })?))
    }
}

/// 全局内置搜索管理器实例
static BUILTIN_SEARCH_MANAGER: once_cell::sync::Lazy<BuiltinSearchManager> = once_cell::sync::Lazy::new(BuiltinSearchManager::new);

/// 获取全局搜索管理器实例
pub fn get_builtin_search_manager() -> &'static BuiltinSearchManager {
    &BUILTIN_SEARCH_MANAGER
}

const DESCRIPTION: &str = r#"Performs web searches and returns concise, relevant results.
Use this tool when search can improve response quality.

You must use the search tool before answering a user's question if it involves any of the following:

1. It relates to "current time / current / present / latest"

2. It involves national leaders, company executives, policies, prices, or statuses

3. The answer may change within one year

You must not provide a final answer before using the search tool."#;

/// 创建搜索工具定义
pub fn create_search_tools() -> Vec<crate::llm::llm::Tool> {
    vec![crate::llm::llm::Tool {
        tool_type: "function".to_string(),
        function: crate::llm::llm::ToolFunction {
            name: "search_web".to_string(),
            description: DESCRIPTION.to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query."
                    },
                    "time_range": {
                        "type": "string",
                        "enum": ["day", "week", "month", "year"],
                        "description": "Filter results by publish/update date. Use 'day' for today's news, 'week' for recent events, etc."
                    }
                },
            }),
        },
    }]
}

/// 判断是否为内置搜索工具调用
pub fn is_builtin_search_tool(tool_name: &str) -> bool {
    matches!(tool_name, "search_web")
}

/// 处理内置搜索工具调用
pub async fn handle_builtin_search_tool(_tool_name: &str, parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    // 根据环境变量选择后端
    if get_search_backend() == "tavily" {
        return tavily_client::handle_search_tool(parameters).await;
    }

    // 默认使用 SearXNG
    let query = parameters
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FunctionCallbackError::InvalidParameters("缺少必需的 'query' 参数".to_string()))?;

    let manager = get_builtin_search_manager();
    let mut options = manager.get_default_options();

    if let Some(engines) = parameters.get("engines")
        && let Some(engines_array) = engines.as_array()
    {
        options.engines = Some(engines_array.iter().filter_map(|e| e.as_str().map(|s| s.to_string())).collect());
    }

    if let Some(language) = parameters.get("language").and_then(|v| v.as_str()) {
        options.language = Some(language.to_string());
    }

    if let Some(time_range) = parameters.get("time_range").and_then(|v| v.as_str()) {
        options.time_range = Some(time_range.to_string());
    }

    if let Some(safe_search) = parameters.get("safe_search").and_then(|v| v.as_u64()) {
        options.safe_search = Some(safe_search as u8);
    }

    manager.search(query, Some(options)).await
}
