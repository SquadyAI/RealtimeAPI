use anyhow::Result;
use rustc_hash::FxHashMap;
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// 工具端点客户端配置
#[derive(Debug, Clone)]
pub struct ToolsEndpointConfig {
    pub endpoint: String,
    pub timeout_secs: u64,
    pub cache_ttl_secs: u64, // 缓存生存时间（秒）
    /// Optional authorization header
    pub authorization: Option<String>,
}

/// 工具端点响应格式
#[derive(Debug, Clone, Deserialize)]
pub struct ToolsEndpointResponse {
    pub tools: Vec<Value>, // 直接存储为Value，支持多种格式
    #[serde(default)]
    pub cache_ttl_secs: Option<u64>, // 服务器建议的缓存TTL
}

/// 缓存的工具数据
#[derive(Debug, Clone)]
struct CachedTools {
    tools: Vec<Value>,
    cached_at: Instant,
    ttl_secs: u64,
}

/// 工具端点客户端
pub struct ToolsEndpointClient {
    config: ToolsEndpointConfig,
    http_client: reqwest::Client,
    cache: RwLock<FxHashMap<String, CachedTools>>, // endpoint -> cached data
}

impl ToolsEndpointClient {
    /// 创建新的工具端点客户端
    pub fn new(config: ToolsEndpointConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .expect("Failed to create HTTP client");

        Self { config, http_client, cache: RwLock::new(FxHashMap::default()) }
    }

    /// 从工具端点获取工具列表
    pub async fn get_tools(&self, endpoint: &str) -> Result<Vec<Value>> {
        // 检查缓存：未过期直接返回；过期则先尝试刷新，失败回退旧数据
        let mut stale: Option<Vec<Value>> = None;
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get(endpoint) {
                if cached.cached_at.elapsed() < Duration::from_secs(cached.ttl_secs) {
                    debug!("从缓存返回工具列表: {} 个工具", cached.tools.len());
                    return Ok(cached.tools.clone());
                } else {
                    stale = Some(cached.tools.clone());
                }
            }
        }

        // 从服务器获取工具列表
        info!("🔧 从工具端点获取工具列表: {}", endpoint);

        let response = self.http_client.get(endpoint).send().await.map_err(|e| {
            if let Some(old) = stale.clone() {
                info!("⚠️ 工具端点请求失败，使用过期缓存: {}", e);
                return anyhow::anyhow!("__USE_STALE__:{}", old.len());
            }
            anyhow::anyhow!("HTTP 工具端点请求失败: {}", e)
        });

        // 如果发送失败但存在过期缓存，立即返回旧数据
        if let Err(err) = &response
            && let Some(old) = stale
        {
            debug!("使用过期工具缓存(请求失败): {}", err);
            return Ok(old);
        }
        let response = response?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if let Some(old) = stale {
                info!("⚠️ 工具端点HTTP错误({}), 使用过期缓存", status);
                return Ok(old);
            }
            return Err(anyhow::anyhow!("HTTP 工具端点请求失败: {} - {}", status, text));
        }

        let response_text = response.text().await.map_err(|e| anyhow::anyhow!("读取响应文本失败: {}", e))?;

        debug!("工具端点原始响应: {}", response_text);

        // 尝试解析响应
        let tools = match self.parse_tools_response(&response_text) {
            Ok(t) => t,
            Err(e) => {
                if let Some(old) = stale {
                    info!("⚠️ 工具端点解析失败，使用过期缓存: {}", e);
                    return Ok(old);
                }
                return Err(e);
            },
        };

        // 更新缓存
        let ttl_secs = self.config.cache_ttl_secs;
        {
            let mut cache = self.cache.write().await;
            cache.insert(
                endpoint.to_string(),
                CachedTools { tools: tools.clone(), cached_at: Instant::now(), ttl_secs },
            );
        }

        info!("✅ 从工具端点获取到 {} 个工具", tools.len());
        Ok(tools)
    }

    /// 解析工具端点响应
    fn parse_tools_response(&self, response_text: &str) -> Result<Vec<Value>> {
        // 尝试解析为包装格式 {tools: [...], cache_ttl_secs: ...}
        if let Ok(response) = serde_json::from_str::<ToolsEndpointResponse>(response_text) {
            info!("✅ 解析为包装格式: {} 个工具", response.tools.len());
            return Ok(response.tools);
        }

        // 尝试解析为工具数组格式 [{type: "function", function: {...}}, ...]
        if let Ok(tools) = serde_json::from_str::<Vec<Value>>(response_text) {
            info!("✅ 解析为工具数组格式: {} 个工具", tools.len());
            return Ok(tools);
        }

        // 尝试解析为其他可能的格式
        if let Ok(json_value) = serde_json::from_str::<Value>(response_text) {
            // 查找可能的工具数组
            let candidates = vec![
                Some(&json_value),
                json_value.get("data"),
                json_value.get("result"),
                json_value.get("payload"),
            ];

            for candidate in candidates {
                if let Some(candidate) = candidate
                    && candidate.is_array()
                    && let Some(tools_array) = candidate.as_array()
                {
                    // 验证是否为有效的工具数组
                    if self.is_valid_tools_array(tools_array) {
                        info!("✅ 解析为兼容格式: {} 个工具", tools_array.len());
                        return Ok(tools_array.to_vec());
                    }
                }
            }
        }

        // 都失败了，返回错误
        let preview: String = response_text.chars().take(400).collect();
        Err(anyhow::anyhow!("无法解析工具端点响应格式。响应预览: {}...", preview))
    }

    /// 验证是否为有效的工具数组
    fn is_valid_tools_array(&self, tools: &[Value]) -> bool {
        if tools.is_empty() {
            return false;
        }

        // 检查第一个工具是否具有基本结构
        if let Some(first_tool) = tools.first()
            && let Some(tool_obj) = first_tool.as_object()
        {
            // 检查是否包含 type 和 function 字段
            return tool_obj.contains_key("type") && tool_obj.contains_key("function");
        }

        false
    }

    /// 清除指定端点的缓存
    pub async fn clear_cache(&self, endpoint: &str) {
        let mut cache = self.cache.write().await;
        cache.remove(endpoint);
        info!("🗑️ 已清除工具端点缓存: {}", endpoint);
    }

    /// 清除所有缓存
    pub async fn clear_all_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        info!("🗑️ 已清除所有工具端点缓存");
    }
}

// 全局工具端点客户端实例
lazy_static::lazy_static! {
    static ref GLOBAL_TOOLS_ENDPOINT_CLIENT: ToolsEndpointClient = {
        let config = ToolsEndpointConfig {
            endpoint: String::new(), // 将在使用时设置
            timeout_secs: 30,
            cache_ttl_secs: 300, // 5分钟缓存
            authorization: None,
        };
        ToolsEndpointClient::new(config)
    };
}

/// 获取全局工具端点客户端
pub fn get_global_tools_endpoint_client() -> &'static ToolsEndpointClient {
    &GLOBAL_TOOLS_ENDPOINT_CLIENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tools_response_wrapped_format() {
        let client = ToolsEndpointClient::new(ToolsEndpointConfig {
            endpoint: "http://test.com".to_string(),
            timeout_secs: 30,
            cache_ttl_secs: 300,
            authorization: None,
        });

        let response = r#"{
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "description": "获取天气信息",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "location": {"type": "string"}
                            },
                            "required": ["location"]
                        }
                    }
                }
            ],
            "cache_ttl_secs": 300
        }"#;

        let tools = client.parse_tools_response(response).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_parse_tools_response_array_format() {
        let client = ToolsEndpointClient::new(ToolsEndpointConfig {
            endpoint: "http://test.com".to_string(),
            timeout_secs: 30,
            cache_ttl_secs: 300,
            authorization: None,
        });

        let response = r#"[
            {
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "获取天气信息",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "location": {"type": "string"}
                        },
                        "required": ["location"]
                    }
                }
            }
        ]"#;

        let tools = client.parse_tools_response(response).unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_is_valid_tools_array() {
        let client = ToolsEndpointClient::new(ToolsEndpointConfig {
            endpoint: "http://test.com".to_string(),
            timeout_secs: 30,
            cache_ttl_secs: 300,
            authorization: None,
        });

        let valid_tools = vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "获取天气信息"
            }
        })];

        assert!(client.is_valid_tools_array(&valid_tools));

        let invalid_tools = vec![serde_json::json!({
            "name": "get_weather"
        })];

        assert!(!client.is_valid_tools_array(&invalid_tools));
    }
}
