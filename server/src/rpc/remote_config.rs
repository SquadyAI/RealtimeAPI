use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::info;

/// 远程配置数据结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemoteConfig {
    /// 系统提示词
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// MCP 服务器配置（数组）
    #[serde(default)]
    pub mcp_server_config: Option<serde_json::Value>,

    /// 工具定义（标准 OpenAI Function Calling 格式）
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,

    /// 搜索引擎配置
    #[serde(default)]
    pub search_config: Option<serde_json::Value>,

    /// 🆕 表情选择提示词（完整的 emoji 选择指令）
    /// 期望JSON格式：
    /// { "emoji_prompt": "你是一个表情选择助手...\n\nAllowed Emojis:\n- 😊 : Happy\n- 😌 : Calm" }
    #[serde(default)]
    pub emoji_prompt: Option<String>,

    /// 🆕 离线工具列表 - 用于拒绝逻辑判断
    /// 支持的离线工具: take_photo, take_video, take_record, music_control, increase_volume, reduce_volume
    /// 兼容蛇形命名 offline_tools 和驼峰命名 offLineTools
    #[serde(default, alias = "offLineTools")]
    pub offline_tools: Option<Vec<String>>,
}

/// 缓存条目
struct CacheEntry {
    config: RemoteConfig,
    created_at: Instant,
}

/// 远程配置客户端
pub struct RemoteConfigClient {
    /// HTTP 客户端
    http_client: reqwest::Client,
    /// 缓存：URL -> (RemoteConfig, Instant)
    cache: Arc<DashMap<String, CacheEntry>>,
    /// 缓存 TTL（秒）
    cache_ttl_secs: u64,
}

impl Default for RemoteConfigClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RemoteConfigClient {
    /// 创建新的远程配置客户端
    pub fn new() -> Self {
        // 从环境变量读取缓存 TTL，默认 300 秒（5 分钟）
        let cache_ttl_secs = std::env::var("REMOTE_CONFIG_CACHE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);

        info!("🔧 初始化远程配置客户端，缓存 TTL: {} 秒", cache_ttl_secs);

        Self {
            http_client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("创建 HTTP 客户端失败"),
            cache: Arc::new(DashMap::new()),
            cache_ttl_secs,
        }
    }

    /// 获取远程配置（带缓存）
    pub async fn get_config(&self, endpoint: &str) -> Result<RemoteConfig, String> {
        // 1. 若命中缓存且未过期：返回缓存
        if let Some(entry) = self.cache.get(endpoint) {
            let elapsed = entry.created_at.elapsed();

            // TTL=0 表示禁用缓存，或者缓存已过期时，需要同步获取
            if self.cache_ttl_secs == 0 || elapsed.as_secs() >= self.cache_ttl_secs {
                info!(
                    "⏰ 缓存已过期或禁用（TTL={}秒，已过{}秒），同步刷新: {}",
                    self.cache_ttl_secs,
                    elapsed.as_secs(),
                    endpoint
                );
                drop(entry); // 释放锁，避免死锁
                // 删除过期缓存，让代码继续执行到缓存未命中的逻辑
                self.cache.remove(endpoint);
                // 不 return，继续执行下面的缓存未命中逻辑
            } else {
                // 缓存有效，直接返回
                info!("🔄 从缓存返回远程配置: {}", endpoint);
                let cached = entry.config.clone();
                return Ok(cached);
            }
        }

        // 2. 缓存未命中或已过期 -> 同步等待加载（确保会话使用正确配置启动）
        info!("🌐 缓存未命中，同步获取远程配置: {}", endpoint);

        let send_res = self.http_client.get(endpoint).header("Accept", "application/json").send().await;

        match send_res {
            Ok(response) => {
                if response.status().is_success() {
                    match response.json::<RemoteConfig>().await {
                        Ok(config) => {
                            // 轻量校验
                            let is_valid = {
                                if let Some(mcp_config) = &config.mcp_server_config {
                                    mcp_config.is_array()
                                } else {
                                    true
                                }
                            } && {
                                if let Some(tools) = &config.tools {
                                    tools.iter().enumerate().all(|(idx, tool)| {
                                        if !tool.is_object() {
                                            info!("⚠️ 配置校验失败: tools[{}] 必须是对象", idx);
                                            return false;
                                        }
                                        let tool_obj = tool.as_object().unwrap();
                                        tool_obj.contains_key("type") && tool_obj.contains_key("function")
                                    })
                                } else {
                                    true
                                }
                            };

                            if is_valid {
                                // 写入缓存
                                self.cache.insert(
                                    endpoint.to_string(),
                                    CacheEntry { config: config.clone(), created_at: Instant::now() },
                                );
                                info!(
                                    "✅ 三合一配置首次获取成功: system_prompt={}, tools={}, mcp={}, search={}",
                                    config.system_prompt.is_some(),
                                    config.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                                    config.mcp_server_config.is_some(),
                                    config.search_config.is_some()
                                );
                                Ok(config)
                            } else {
                                let err = format!("配置验证失败: {}", endpoint);
                                info!("⚠️ {}", err);
                                Err(err)
                            }
                        },
                        Err(e) => {
                            let err = format!("JSON 解析失败: {}", e);
                            info!("⚠️ {}", err);
                            Err(err)
                        },
                    }
                } else {
                    let err = format!("HTTP 错误: {} -> {}", response.status(), endpoint);
                    info!("⚠️ {}", err);
                    Err(err)
                }
            },
            Err(e) => {
                let err = format!("请求失败: {}", e);
                info!("⚠️ {}", err);
                Err(err)
            },
        }
    }

    /// 验证远程配置格式
    #[allow(dead_code)]
    fn validate_config(&self, config: &RemoteConfig) -> Result<(), String> {
        // 验证 mcp_server_config 必须是数组（如果存在）
        if let Some(mcp_config) = &config.mcp_server_config
            && !mcp_config.is_array()
        {
            return Err("mcp_server_config 必须是数组类型".to_string());
        }

        // 验证 tools 格式（如果存在）
        if let Some(tools) = &config.tools {
            for (idx, tool) in tools.iter().enumerate() {
                // 检查是否有 type 和 function 字段
                if !tool.is_object() {
                    return Err(format!("tools[{}] 必须是对象", idx));
                }

                let tool_obj = tool.as_object().unwrap();
                if !tool_obj.contains_key("type") || !tool_obj.contains_key("function") {
                    return Err(format!("tools[{}] 缺少 'type' 或 'function' 字段", idx));
                }
            }
        }

        Ok(())
    }

    /// 清除缓存
    pub fn clear_cache(&self) {
        self.cache.clear();
        info!("🗑️ 远程配置缓存已清空");
    }

    /// 清除指定端点的缓存
    pub fn clear_cache_for(&self, endpoint: &str) {
        self.cache.remove(endpoint);
        info!("🗑️ 已清除缓存: {}", endpoint);
    }

    /// 获取缓存统计信息
    pub fn get_cache_stats(&self) -> (usize, usize) {
        let total = self.cache.len();
        let valid = self
            .cache
            .iter()
            .filter(|entry| entry.created_at.elapsed().as_secs() < self.cache_ttl_secs)
            .count();
        (total, valid)
    }
}

/// 全局远程配置客户端实例
static GLOBAL_REMOTE_CONFIG_CLIENT: Lazy<RemoteConfigClient> = Lazy::new(RemoteConfigClient::new);

/// 获取全局远程配置客户端
pub fn get_global_remote_config_client() -> &'static RemoteConfigClient {
    &GLOBAL_REMOTE_CONFIG_CLIENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remote_config_deserialization() {
        let json = r#"{
            "system_prompt": "你是 AI 助手",
            "mcp_server_config": [
                {
                    "endpoint": "https://example.com/mcp",
                    "timeout_secs": 30
                }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "test_tool",
                        "description": "测试工具",
                        "parameters": {
                            "type": "object",
                            "properties": {}
                        }
                    }
                }
            ],
            "search_config": null
        }"#;

        let config: RemoteConfig = serde_json::from_str(json).unwrap();
        assert!(config.system_prompt.is_some());
        assert!(config.mcp_server_config.is_some());
        assert_eq!(config.tools.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_remote_config_minimal() {
        let json = r#"{
            "system_prompt": "你是 AI 助手"
        }"#;

        let config: RemoteConfig = serde_json::from_str(json).unwrap();
        assert!(config.system_prompt.is_some());
        assert!(config.mcp_server_config.is_none());
        assert!(config.tools.is_none());
        assert!(config.search_config.is_none());
    }

    #[test]
    fn test_validate_config() {
        let client = RemoteConfigClient::new();

        // 正确的配置
        let valid_config = RemoteConfig {
            emoji_prompt: None,
            system_prompt: Some("test".to_string()),
            mcp_server_config: Some(serde_json::json!([])),
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {
                    "name": "test",
                    "description": "test",
                    "parameters": {}
                }
            })]),
            search_config: None,
            offline_tools: None,
        };
        assert!(client.validate_config(&valid_config).is_ok());

        // mcp_server_config 不是数组
        let invalid_config = RemoteConfig {
            system_prompt: None,
            emoji_prompt: None,
            mcp_server_config: Some(serde_json::json!({})),
            tools: None,
            search_config: None,
            offline_tools: None,
        };
        assert!(client.validate_config(&invalid_config).is_err());
    }
}
