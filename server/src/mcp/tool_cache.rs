use crate::mcp::{HttpMcpClient, McpError, McpTool};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// 工具缓存条目
#[derive(Debug, Clone)]
struct ToolCacheEntry {
    /// 缓存的工具列表
    tools: Vec<McpTool>,
    /// 缓存创建时间
    cached_at: Instant,
    /// 缓存TTL（秒）
    ttl_secs: u64,
    /// 最后访问时间
    last_accessed: Instant,
    /// 访问计数
    access_count: usize,
}

impl ToolCacheEntry {
    fn new(tools: Vec<McpTool>, ttl_secs: u64) -> Self {
        let now = Instant::now();
        Self { tools, cached_at: now, ttl_secs, last_accessed: now, access_count: 0 }
    }

    /// 检查缓存是否过期
    fn is_expired(&self) -> bool {
        self.cached_at.elapsed() > Duration::from_secs(self.ttl_secs)
    }

    /// 更新访问时间和计数
    fn mark_accessed(&mut self) {
        self.last_accessed = Instant::now();
        self.access_count += 1;
    }

    /// 获取缓存剩余时间（秒）
    fn remaining_ttl_secs(&self) -> u64 {
        let elapsed = self.cached_at.elapsed().as_secs();
        self.ttl_secs.saturating_sub(elapsed)
    }
}

/// 全局MCP工具缓存管理器
#[derive(Debug)]
pub struct GlobalMcpToolCache {
    /// 缓存存储：endpoint URL -> 工具缓存条目
    cache: Arc<RwLock<FxHashMap<String, ToolCacheEntry>>>,
    /// 默认缓存TTL（秒）
    default_ttl_secs: u64,
    /// 缓存清理间隔（秒）
    cleanup_interval_secs: u64,
    /// 最大缓存条目数
    max_cache_entries: usize,
}

impl GlobalMcpToolCache {
    /// 创建新的全局工具缓存管理器
    pub fn new(default_ttl_secs: u64, cleanup_interval_secs: u64, max_cache_entries: usize) -> Self {
        let cache = Self {
            cache: Arc::new(RwLock::new(FxHashMap::default())),
            default_ttl_secs,
            cleanup_interval_secs,
            max_cache_entries,
        };

        // 启动定期清理任务
        cache.start_cleanup_task();

        cache
    }

    /// 从缓存获取工具列表
    pub async fn get_tools(&self, endpoint: &str) -> Option<Vec<McpTool>> {
        let mut cache = self.cache.write().await;

        if let Some(entry) = cache.get_mut(endpoint) {
            let expired = entry.is_expired();
            entry.mark_accessed();

            if expired {
                // 不再清理过期项：返回旧数据，交由调用方决定刷新策略
                debug!(
                    "🕐 MCP工具缓存已过期(返回旧数据): endpoint={}, 工具数={}, 过期{}s, 访问次数={}",
                    endpoint,
                    entry.tools.len(),
                    entry.cached_at.elapsed().as_secs().saturating_sub(entry.ttl_secs),
                    entry.access_count
                );
            } else {
                debug!(
                    "🎯 从缓存返回MCP工具: endpoint={}, 工具数={}, 剩余TTL={}s, 访问次数={}",
                    endpoint,
                    entry.tools.len(),
                    entry.remaining_ttl_secs(),
                    entry.access_count
                );
            }

            return Some(entry.tools.clone());
        }

        None
    }

    /// 缓存工具列表
    pub async fn cache_tools(&self, endpoint: &str, tools: Vec<McpTool>, ttl_secs: Option<u64>) -> Result<(), McpError> {
        let ttl = ttl_secs.unwrap_or(self.default_ttl_secs);
        let mut cache = self.cache.write().await;

        // 检查缓存大小限制
        if cache.len() >= self.max_cache_entries && !cache.contains_key(endpoint) {
            // 清理最老的缓存条目
            self.evict_oldest_entry(&mut cache).await;
        }

        let entry = ToolCacheEntry::new(tools.clone(), ttl);
        cache.insert(endpoint.to_string(), entry);

        info!(
            "💾 缓存MCP工具列表: endpoint={}, 工具数={}, TTL={}s",
            endpoint,
            tools.len(),
            ttl
        );

        Ok(())
    }

    /// 获取HTTP MCP工具列表（带缓存）
    pub async fn get_http_mcp_tools(&self, http_client: &HttpMcpClient) -> Result<Vec<McpTool>, McpError> {
        let endpoint = http_client.get_url();

        // 直接读取内部状态，判断是否过期
        let (maybe_tools, is_expired) = {
            let mut cache = self.cache.write().await;
            if let Some(entry) = cache.get_mut(endpoint) {
                let expired = entry.is_expired();
                entry.mark_accessed();
                (Some(entry.tools.clone()), expired)
            } else {
                (None, true)
            }
        };

        // 未命中或已过期：尝试立即刷新；刷新失败且有旧数据则回退旧数据
        if is_expired || maybe_tools.is_none() {
            info!(
                "🔄 HTTP MCP工具缓存{}，从服务器刷新: endpoint={}",
                if maybe_tools.is_some() { "已过期" } else { "未命中" },
                endpoint
            );

            match http_client.get_tools().await {
                Ok(tools_response) => {
                    info!(
                        "✅ HTTP MCP 工具刷新成功: endpoint={}, 工具数={}",
                        endpoint,
                        tools_response.tools.len()
                    );
                    let server_ttl = tools_response.cache_ttl_secs;
                    self.cache_tools(endpoint, tools_response.tools.clone(), server_ttl).await?;
                    return Ok(tools_response.tools);
                },
                Err(e) => {
                    error!("❌ HTTP MCP 工具刷新失败: endpoint={}, error={}", endpoint, e);
                    if let Some(stale) = maybe_tools {
                        warn!("⚠️ 使用过期的MCP工具缓存(HTTP): endpoint={}, 工具数={}", endpoint, stale.len());
                        return Ok(stale);
                    }
                    return Err(McpError::ConnectionError(format!("HTTP MCP工具列表获取失败: {}", e)));
                },
            }
        }

        // 命中且未过期：直接返回
        let tools = maybe_tools.unwrap_or_default();
        info!("✅ 从缓存获取MCP工具: endpoint={}, 工具数={}", endpoint, tools.len());
        Ok(tools)
    }

    /// 强制刷新指定endpoint的缓存
    pub async fn refresh_cache(&self, endpoint: &str) -> Result<(), McpError> {
        let mut cache = self.cache.write().await;
        if cache.remove(endpoint).is_some() {
            info!("🔄 已清除MCP工具缓存: endpoint={}", endpoint);
        }
        Ok(())
    }

    /// 清除所有缓存
    pub async fn clear_all_cache(&self) -> Result<(), McpError> {
        let mut cache = self.cache.write().await;
        let count = cache.len();
        cache.clear();
        info!("🧹 已清除所有MCP工具缓存: {} 个条目", count);
        Ok(())
    }

    /// 获取缓存统计信息
    pub async fn get_cache_stats(&self) -> FxHashMap<String, serde_json::Value> {
        let cache = self.cache.read().await;
        let mut stats = FxHashMap::default();

        stats.insert("total_entries".to_string(), serde_json::Value::Number(cache.len().into()));
        stats.insert(
            "max_entries".to_string(),
            serde_json::Value::Number(self.max_cache_entries.into()),
        );
        stats.insert(
            "default_ttl_secs".to_string(),
            serde_json::Value::Number(self.default_ttl_secs.into()),
        );

        let mut expired_count = 0;
        let mut total_tools = 0;
        let mut total_access_count = 0;
        let mut endpoints = Vec::new();

        for (endpoint, entry) in cache.iter() {
            if entry.is_expired() {
                expired_count += 1;
            }
            total_tools += entry.tools.len();
            total_access_count += entry.access_count;

            endpoints.push(serde_json::json!({
                "endpoint": endpoint,
                "tools_count": entry.tools.len(),
                "cached_at": entry.cached_at.elapsed().as_secs(),
                "ttl_secs": entry.ttl_secs,
                "remaining_ttl_secs": entry.remaining_ttl_secs(),
                "access_count": entry.access_count,
                "is_expired": entry.is_expired(),
            }));
        }

        stats.insert("expired_entries".to_string(), serde_json::Value::Number(expired_count.into()));
        stats.insert("total_tools".to_string(), serde_json::Value::Number(total_tools.into()));
        stats.insert(
            "total_access_count".to_string(),
            serde_json::Value::Number(total_access_count.into()),
        );
        stats.insert("endpoints".to_string(), serde_json::Value::Array(endpoints));

        stats
    }

    /// 清理过期缓存条目
    #[allow(dead_code)]
    async fn cleanup_expired_entries(&self) {
        // 自然失效：直接移除过期条目
        let mut cache = self.cache.write().await;
        let before = cache.len();
        cache.retain(|_k, v| !v.is_expired());
        let removed = before.saturating_sub(cache.len());
        if removed > 0 {
            info!("🧹 cleanup_expired_entries: 已移除 {} 个过期MCP工具缓存条目", removed);
        }
    }

    /// 驱逐最老的缓存条目
    async fn evict_oldest_entry(&self, cache: &mut FxHashMap<String, ToolCacheEntry>) {
        if let Some((oldest_endpoint, _)) = cache.iter().min_by_key(|(_, entry)| entry.last_accessed) {
            let oldest_endpoint = oldest_endpoint.clone();
            cache.remove(&oldest_endpoint);
            warn!("🧹 缓存已满，驱逐最老的条目: endpoint={}", oldest_endpoint);
        }
    }

    /// 启动定期清理任务
    fn start_cleanup_task(&self) {
        let cache_clone = self.cache.clone();
        let cleanup_interval = Duration::from_secs(self.cleanup_interval_secs);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);

            loop {
                interval.tick().await;

                // 自然失效：定期移除已过期的条目，避免无界增长
                let mut cache = cache_clone.write().await;
                let before = cache.len();
                cache.retain(|_k, v| !v.is_expired());
                let removed = before.saturating_sub(cache.len());
                if removed > 0 {
                    info!("🧹 已移除 {} 个过期的MCP工具缓存条目 (自然失效)", removed);
                }
            }
        });
    }

    /// 返回工具及其过期状态（供调用方决定是否刷新）
    pub async fn get_tools_with_status(&self, endpoint: &str) -> Option<(Vec<McpTool>, bool)> {
        let mut cache = self.cache.write().await;
        if let Some(entry) = cache.get_mut(endpoint) {
            let expired = entry.is_expired();
            entry.mark_accessed();
            return Some((entry.tools.clone(), expired));
        }
        None
    }
}

impl Default for GlobalMcpToolCache {
    fn default() -> Self {
        // 允许通过环境变量覆盖最大条目数
        let max_entries = std::env::var("MCP_TOOL_CACHE_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100_000);
        Self::new(
            300, // 默认5分钟TTL
            60,  // 每分钟清理一次
            max_entries,
        )
    }
}

/// 创建全局单例实例
use once_cell::sync::Lazy;

pub static GLOBAL_MCP_TOOL_CACHE: Lazy<GlobalMcpToolCache> = Lazy::new(GlobalMcpToolCache::default);
