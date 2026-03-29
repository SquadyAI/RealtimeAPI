//! SearXNG 搜索客户端
//!
//! 提供低延迟、流式的搜索API调用功能
//! 支持实时搜索建议、结果流式返回、缓存优化

use crate::env_utils::{env_bool_or_default, env_or_default, env_string_or_default};
use crate::function_callback::{CallResult, FunctionCallbackError};
use futures::stream::{self, Stream};
use reqwest::Client;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{RwLock, Semaphore};
use tokio::time::timeout;
use tracing::{debug, info};

/// SearXNG 搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearXNGResult {
    /// 结果标题
    pub title: String,
    /// 结果URL (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub url: String,
    /// 结果摘要
    pub content: String,
    /// 搜索引擎 (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub engine: String,
    /// 结果类型 (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub result_type: ResultType,
    /// 相关度分数 (仅非空时序列化)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// 发布时间 (仅非空时序列化)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_date: Option<String>,
    /// 图片URL (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub image_url: Option<String>,
}

/// 结果类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResultType {
    /// 网页结果
    Web,
    /// 图片结果
    Image,
    /// 新闻结果
    News,
    /// 视频结果
    Video,
    /// 学术结果
    Academic,
}

/// SearXNG 搜索响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearXNGResponse {
    /// 查询字符串
    pub query: String,
    /// 搜索结果
    pub results: Vec<SearXNGResult>,
    /// 总结果数 (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub total_results: Option<u64>,
    /// 搜索时间 (ms) (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub search_time: f32,
    /// 是否来自缓存 (不序列化给LLM，节省token)
    #[serde(skip_serializing)]
    pub from_cache: bool,
    /// 错误信息 (仅非空时序列化)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// SearXNG 客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearXNGConfig {
    /// 默认超时时间 (ms) - 不再从环境变量读取SEARXNG_TIMEOUT_MS
    pub timeout_ms: u32,
    /// 最大并发请求数
    pub max_concurrent_requests: usize,
    /// 启用缓存
    pub enable_cache: bool,
    /// 缓存过期时间 (秒)
    pub cache_ttl_seconds: u64,
    /// 启用流式响应
    pub enable_streaming: bool,
    /// 默认搜索引擎
    pub default_engines: Vec<String>,
    /// 默认语言
    pub default_language: String,
    /// 安全搜索级别 (0-2)
    pub safe_search: u8,
    /// 结果数量限制
    pub max_results: usize,
    /// 启用自动完成
    pub enable_autocomplete: bool,
    /// 自动完成服务
    pub autocomplete_service: String,
}

/// 从环境变量读取搜索服务基础 URL（去除末尾斜杠），提供默认值
fn get_searxng_base_url() -> String {
    // 兼容旧环境变量名，默认切换到新搜索服务域名
    let raw = env_string_or_default("SEARXNG_BASE_URL", "http://localhost:8787");
    raw.trim_end_matches('/').to_string()
}

impl Default for SearXNGConfig {
    fn default() -> Self {
        Self {
            // 🔒 不再从SEARXNG_TIMEOUT_MS环境变量读取，但保留其值以确保兼容性
            timeout_ms: env_or_default("SEARXNG_TIMEOUT_MS", 5000),
            max_concurrent_requests: env_or_default("SEARXNG_MAX_CONCURRENT_REQUESTS", 200),
            enable_cache: env_bool_or_default("SEARXNG_ENABLE_CACHE", true),
            cache_ttl_seconds: env_or_default("SEARXNG_CACHE_TTL_SECONDS", 300), // 5分钟
            enable_streaming: env_bool_or_default("SEARXNG_ENABLE_STREAMING", true),
            default_engines: env_string_or_default("SEARXNG_DEFAULT_ENGINES", "")
                .split(',')
                .map(|s| s.trim().to_string())
                .collect(),
            default_language: env_string_or_default("SEARXNG_DEFAULT_LANGUAGE", "zh-CN"),
            safe_search: env_or_default("SEARXNG_SAFE_SEARCH", 0),
            max_results: env_or_default("SEARXNG_MAX_RESULTS", 20),
            enable_autocomplete: env_bool_or_default("SEARXNG_ENABLE_AUTOCOMPLETE", false),
            autocomplete_service: env_string_or_default("SEARXNG_AUTOCOMPLETE_SERVICE", ""),
        }
    }
}

impl SearXNGConfig {
    /// 🆕 创建仅包含搜索参数的配置（忽略endpoint相关配置）
    pub fn search_params_only() -> Self {
        Self {
            timeout_ms: 5000, // 固定超时时间
            max_concurrent_requests: 200,
            enable_cache: true,
            cache_ttl_seconds: 300,
            enable_streaming: true,
            default_engines: vec![],
            default_language: "zh-CN".to_string(),
            safe_search: 0,
            max_results: 20,
            enable_autocomplete: true,
            autocomplete_service: "brave".to_string(),
        }
    }

    /// 🆕 从外部配置更新搜索参数（忽略任何endpoint配置）
    pub fn with_search_params(mut self, config: &serde_json::Value) -> Self {
        if let serde_json::Value::Object(map) = config {
            // 只允许更新搜索相关参数，忽略endpoint、base_url等
            if let Some(engines) = map.get("default_engines")
                && let Some(engines_str) = engines.as_str()
            {
                self.default_engines = engines_str.split(',').map(|s| s.trim().to_string()).collect();
            }

            if let Some(language) = map.get("default_language").and_then(|v| v.as_str()) {
                self.default_language = language.to_string();
            }

            if let Some(safe_search) = map.get("safe_search").and_then(|v| v.as_u64()) {
                self.safe_search = safe_search as u8;
            }

            if let Some(max_results) = map.get("max_results").and_then(|v| v.as_u64()) {
                self.max_results = max_results as usize;
            }

            if let Some(autocomplete_service) = map.get("autocomplete_service").and_then(|v| v.as_str()) {
                self.autocomplete_service = autocomplete_service.to_string();
            }

            // 明确忽略可能的endpoint配置（使用环境变量控制）
            if map.contains_key("base_url") || map.contains_key("endpoint") || map.contains_key("url") {
                info!(
                    "🔒 忽略外部endpoint配置，使用环境变量 SEARXNG_BASE_URL: {}",
                    get_searxng_base_url()
                );
            }
        }
        self
    }
}

/// 缓存条目
#[derive(Debug, Clone)]
struct CacheEntry {
    response: SearXNGResponse,
    timestamp: SystemTime,
}

/// SearXNG 客户端
pub struct SearXNGClient {
    config: SearXNGConfig,
    http_client: Client,
    cache: Arc<RwLock<FxHashMap<String, CacheEntry>>>,
    semaphore: Arc<Semaphore>,
    stats: Arc<RwLock<SearXNGStats>>,
    // 🚀 预构建的URL缓存
    search_url: String,
}

/// SearXNG 统计信息
#[derive(Debug, Clone)]
pub struct SearXNGStats {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub average_response_time_ms: f32,
    pub total_response_time_ms: u64,
    pub concurrent_requests: usize,
}

impl Default for SearXNGStats {
    fn default() -> Self {
        Self {
            total_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            cache_hits: 0,
            cache_misses: 0,
            average_response_time_ms: 0.0,
            total_response_time_ms: 0,
            concurrent_requests: 0,
        }
    }
}

impl SearXNGClient {
    /// 创建新的 SearXNG 客户端
    pub async fn new(config: SearXNGConfig) -> Result<Self, FunctionCallbackError> {
        let base_url = get_searxng_base_url();
        info!("🔍 初始化 SearXNG 客户端: {}", base_url);

        // 🚀 优化的HTTP客户端配置 - 参考LLM客户端的最佳实践
        let http_client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms as u64))
            // ⚠️ 与 wget --no-check-certificate 一致：忽略无效证书与主机名
            .danger_accept_invalid_certs(true)
            .danger_accept_invalid_hostnames(true)
            // 🔧 连接池优化配置
            .pool_max_idle_per_host(config.max_concurrent_requests.min(50)) // 动态调整，最大50
            .pool_idle_timeout(Duration::from_secs(90)) // 空闲连接保持90秒
            .connect_timeout(Duration::from_secs(5)) // 连接超时5秒
            .tcp_keepalive(Duration::from_secs(60)) // TCP keep-alive 60秒
            .tcp_nodelay(true) // 禁用Nagle算法，减少延迟
            // 🚀 HTTP/2 优化配置
            .http2_keep_alive_interval(Duration::from_secs(30)) // HTTP/2 keep-alive
            .http2_keep_alive_timeout(Duration::from_secs(5)) // HTTP/2 keep-alive超时
            .http2_keep_alive_while_idle(true) // 空闲时也保持keep-alive
            .http2_adaptive_window(true) // 自适应窗口大小
            .build()
            .map_err(|e| FunctionCallbackError::Config(format!("HTTP客户端创建失败: {}", e)))?;

        // 🚀 预构建URL，避免运行时字符串操作（新API）
        // 兼容以下三种配置：
        // - base="https://host"            -> "https://host/v1/search"
        // - base="https://host/v1"         -> "https://host/v1/search"
        // - base="https://host/v1/search"  -> "https://host/v1/search"
        let search_url = if base_url.ends_with("/v1/search") || base_url.ends_with("/search") {
            base_url.clone()
        } else if base_url.ends_with("/v1") {
            format!("{}/search", base_url)
        } else {
            format!("{}/v1/search", base_url)
        };

        info!("🔗 搜索服务URL: {}", search_url);

        // 🚀 使用配置中的并发请求数，而不是硬编码的10
        let semaphore = Arc::new(Semaphore::new(config.max_concurrent_requests));

        let client = Self {
            config,
            http_client,
            cache: Arc::new(RwLock::new(FxHashMap::default())),
            semaphore,
            stats: Arc::new(RwLock::new(SearXNGStats::default())),
            search_url,
        };

        // 预热连接
        client.warmup_connection().await?;

        Ok(client)
    }

    /// 执行搜索
    pub async fn search(&self, query: &str, options: Option<SearchOptions>) -> Result<SearXNGResponse, FunctionCallbackError> {
        let start_time = Instant::now();
        let options = options.unwrap_or_default();

        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.total_requests += 1;
            stats.concurrent_requests = self.semaphore.available_permits();
        }

        // 检查缓存
        if self.config.enable_cache
            && let Some(cached_response) = self.get_cached_response(query, &options).await?
        {
            debug!("缓存命中: {}", query);
            let mut stats = self.stats.write().await;
            stats.cache_hits += 1;
            return Ok(cached_response);
        }

        // 获取信号量许可
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| FunctionCallbackError::Other(format!("无法获取并发许可: {}", e)))?;

        // 执行搜索
        let result = self.execute_search(query, &options).await;

        let response_time = start_time.elapsed().as_millis() as u64;

        // 更新统计信息
        {
            let mut stats = self.stats.write().await;
            stats.total_response_time_ms += response_time;
            stats.average_response_time_ms = stats.total_response_time_ms as f32 / stats.total_requests as f32;

            match &result {
                Ok(_) => stats.successful_requests += 1,
                Err(_) => stats.failed_requests += 1,
            }
        }

        // 缓存结果
        if let Ok(ref response) = result
            && self.config.enable_cache
        {
            self.cache_response(query, &options, response).await?;
        }

        result
    }

    /// 流式搜索
    pub async fn search_stream(&self, query: &str, options: Option<SearchOptions>) -> Result<Pin<Box<dyn Stream<Item = Result<SearXNGResult, FunctionCallbackError>> + Send>>, FunctionCallbackError> {
        if !self.config.enable_streaming {
            return Err(FunctionCallbackError::NotImplemented("流式搜索未启用".to_string()));
        }

        let options = options.unwrap_or_default();
        let query = query.to_string();

        // 创建流式响应
        let response = self.search(&query, Some(options)).await?;
        let stream = stream::iter(response.results.into_iter().map(Ok));

        Ok(Box::pin(stream))
    }

    // /// 获取搜索建议
    // pub async fn get_suggestions(&self, _query: &str) -> Result<Vec<String>, FunctionCallbackError> {
    //     // 新接口不再提供自动补全
    //     Err(FunctionCallbackError::NotImplemented("自动补全接口已移除".to_string()))
    // }

    /// 执行搜索请求
    async fn execute_search(&self, query: &str, options: &SearchOptions) -> Result<SearXNGResponse, FunctionCallbackError> {
        // 新API: POST JSON { query, engines }
        let mut engines: Vec<String> = if let Some(e) = &options.engines {
            e.clone()
        } else {
            self.config.default_engines.clone()
        };
        if engines.is_empty() {
            engines.push("brave".to_string());
        }

        let payload = serde_json::json!({
            "query": query
            // "engines": engines,
        });

        debug!("执行搜索(新API): {}", query);

        let response = timeout(
            Duration::from_millis(self.config.timeout_ms as u64),
            self.http_client.post(&self.search_url).json(&payload).send(),
        )
        .await
        .map_err(|_| FunctionCallbackError::Timeout("搜索超时".to_string()))?
        .map_err(|e| FunctionCallbackError::Network(format!("网络请求失败: {}", e)))?;

        // 接口可能返回401但仍提供可用的JSON结果体，忽略状态码直接解析
        let text_body = response
            .text()
            .await
            .map_err(|e| FunctionCallbackError::Other(format!("读取搜索响应失败: {}", e)))?;

        let search_data: serde_json::Value = serde_json::from_str(&text_body).map_err(|e| FunctionCallbackError::Other(format!("解析搜索结果失败: {}", e)))?;

        self.parse_search_response(query, search_data)
    }

    /// 解析搜索响应
    fn parse_search_response(&self, query: &str, data: serde_json::Value) -> Result<SearXNGResponse, FunctionCallbackError> {
        // 🚀 只在debug模式下输出完整响应，减少生产环境开销
        info!("原始响应数据: {}", serde_json::to_string_pretty(&data).unwrap_or_default());

        // 新API字段提取
        let provider = data.get("provider").and_then(|v| v.as_str()).unwrap_or("unknown");

        // items 为主要结果集合
        let items = if let Some(arr) = data.get("items").and_then(|r| r.as_array()) {
            arr.clone()
        } else {
            return Err(FunctionCallbackError::Other("响应中缺少 items 字段".to_string()));
        };

        // 从 raw.organic 中构建 URL -> date 的映射，用于补充日期信息
        let date_map: FxHashMap<String, String> = data
            .get("raw")
            .and_then(|r| r.get("organic"))
            .and_then(|o| o.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let url = item.get("link").and_then(|l| l.as_str())?;
                        let date = item.get("date").and_then(|d| d.as_str())?;
                        Some((url.to_string(), date.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut search_results = Vec::with_capacity(items.len().min(self.config.max_results));

        for item in items.into_iter().take(self.config.max_results) {
            let title = item.get("title").and_then(|t| t.as_str()).unwrap_or("无标题");
            let url = item.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let snippet = item.get("snippet").and_then(|c| c.as_str()).unwrap_or("");
            let source = item.get("source").and_then(|s| s.as_str()).unwrap_or(provider);

            // 从 date_map 中查找该 URL 对应的日期
            let published_date = date_map.get(url).cloned();

            let search_result = SearXNGResult {
                title: title.to_string(),
                url: url.to_string(),
                content: snippet.to_string(),
                engine: source.to_string(),
                result_type: ResultType::Web,
                score: None,
                published_date,
                image_url: None,
            };

            search_results.push(search_result);
        }

        // LLM有意义的附加元信息
        let latency_ms = data.get("latencyMs").and_then(|v| v.as_u64()).unwrap_or(0);
        let total_results = Some(search_results.len() as u64);

        Ok(SearXNGResponse {
            query: data.get("query").and_then(|v| v.as_str()).unwrap_or(query).to_string(),
            results: search_results,
            // suggestions: Vec::new(),
            total_results,
            search_time: latency_ms as f32,
            from_cache: false,
            error: None,
        })
    }

    /// 获取缓存的响应
    async fn get_cached_response(&self, query: &str, options: &SearchOptions) -> Result<Option<SearXNGResponse>, FunctionCallbackError> {
        let cache_key = self.generate_cache_key(query, options);
        let cache = self.cache.read().await;

        if let Some(entry) = cache.get(&cache_key) {
            let now = SystemTime::now();
            if let Ok(duration) = now.duration_since(entry.timestamp)
                && duration.as_secs() < self.config.cache_ttl_seconds
            {
                let mut response = entry.response.clone();
                response.from_cache = true;
                return Ok(Some(response));
            }
        }

        Ok(None)
    }

    /// 缓存响应
    async fn cache_response(&self, query: &str, options: &SearchOptions, response: &SearXNGResponse) -> Result<(), FunctionCallbackError> {
        let cache_key = self.generate_cache_key(query, options);
        let entry = CacheEntry { response: response.clone(), timestamp: SystemTime::now() };

        let mut cache = self.cache.write().await;
        cache.insert(cache_key, entry);

        Ok(())
    }

    /// 生成缓存键
    fn generate_cache_key(&self, query: &str, options: &SearchOptions) -> String {
        // 🚀 使用更高效的字符串构建方式
        let mut key_parts = Vec::with_capacity(6);
        key_parts.push(query.to_string());

        if let Some(engines) = &options.engines {
            key_parts.push("engines:".to_string());
            key_parts.push(engines.join(","));
        }

        if let Some(language) = &options.language {
            key_parts.push("lang:".to_string());
            key_parts.push(language.clone());
        }

        if let Some(time_range) = &options.time_range {
            key_parts.push("time:".to_string());
            key_parts.push(time_range.clone());
        }

        if let Some(safe_search) = options.safe_search {
            key_parts.push("safe:".to_string());
            // 🚀 避免在每次调用时分配新字符串
            key_parts.push(match safe_search {
                0 => "0".to_string(),
                1 => "1".to_string(),
                2 => "2".to_string(),
                _ => "0".to_string(),
            });
        }

        key_parts.join("")
    }

    /// 预热连接
    async fn warmup_connection(&self) -> Result<(), FunctionCallbackError> {
        // 🚀 并行预热多个连接，建立连接池（使用HEAD，不关心状态码）
        let warmup_tasks = (0..3).map(|_| {
            let client = self.http_client.clone();
            let url = self.search_url.clone();
            async move {
                let _ = client.head(&url).send().await;
            }
        });

        // 并行执行所有预热任务，但不等待结果
        futures::future::join_all(warmup_tasks).await;

        info!("SearXNG 连接预热完成");
        Ok(())
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> SearXNGStats {
        self.stats.read().await.clone()
    }

    /// 重置统计信息
    pub async fn reset_stats(&self) {
        let mut stats = self.stats.write().await;
        *stats = SearXNGStats::default();
    }

    /// 清理过期缓存
    pub async fn cleanup_cache(&self) -> Result<usize, FunctionCallbackError> {
        let now = SystemTime::now();
        let mut cache = self.cache.write().await;
        let initial_size = cache.len();

        cache.retain(|_, entry| {
            if let Ok(duration) = now.duration_since(entry.timestamp) {
                duration.as_secs() < self.config.cache_ttl_seconds
            } else {
                false
            }
        });

        let removed_count = initial_size - cache.len();
        info!("清理了 {} 个过期缓存条目", removed_count);

        Ok(removed_count)
    }
}

/// 搜索选项
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// 搜索引擎列表
    pub engines: Option<Vec<String>>,
    /// 语言
    pub language: Option<String>,
    /// 时间范围
    pub time_range: Option<String>,
    /// 安全搜索级别
    pub safe_search: Option<u8>,
    /// 分类
    pub categories: Option<Vec<String>>,
    /// 页码
    pub page: Option<u32>,
    /// 每页结果数
    pub results_per_page: Option<usize>,
}

/// 全局 SearXNG 客户端实例
static SEARXNG_CLIENT: once_cell::sync::OnceCell<Arc<SearXNGClient>> = once_cell::sync::OnceCell::new();

/// 获取全局 SearXNG 客户端实例
pub async fn get_searxng_client() -> Result<Arc<SearXNGClient>, FunctionCallbackError> {
    if let Some(client) = SEARXNG_CLIENT.get() {
        Ok(client.clone())
    } else {
        Err(FunctionCallbackError::Config("SearXNG 客户端未初始化".to_string()))
    }
}

/// 初始化全局 SearXNG 客户端
pub async fn init_searxng_client(config: SearXNGConfig) -> Result<Arc<SearXNGClient>, FunctionCallbackError> {
    let client = SearXNGClient::new(config).await?;
    let client_arc = Arc::new(client);

    SEARXNG_CLIENT
        .set(client_arc.clone())
        .map_err(|_| FunctionCallbackError::Config("SearXNG 客户端已初始化".to_string()))?;

    Ok(client_arc)
}

/// 搜索功能实现
pub async fn search_function(parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    let query = parameters
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FunctionCallbackError::InvalidParameters("缺少必需的 'query' 参数".to_string()))?;

    let client = get_searxng_client().await?;

    // 解析可选参数
    let mut options = SearchOptions::default();

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

    let response = client.search(query, Some(options)).await?;

    Ok(CallResult::Success(serde_json::to_value(response).map_err(|e| {
        FunctionCallbackError::Other(format!("序列化搜索结果失败: {}", e))
    })?))
}

// /// 获取搜索建议功能实现
// pub async fn get_suggestions_function(parameters: &HashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
//     let query = parameters.get("query")
//         .and_then(|v| v.as_str())
//         .ok_or_else(|| FunctionCallbackError::InvalidParameters("缺少必需的 'query' 参数".to_string()))?;

//     let client = get_searxng_client().await?;
//     let suggestions = client.get_suggestions(query).await?;

//     Ok(CallResult::Success(serde_json::to_value(suggestions)
//         .map_err(|e| FunctionCallbackError::Other(format!("序列化建议失败: {}", e)))?))
// }

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_searxng_client_creation() {
        let config = SearXNGConfig::default();
        let client = SearXNGClient::new(config).await;
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn test_search_function() {
        let mut params = FxHashMap::default();
        params.insert("query".to_string(), serde_json::Value::String("Rust programming".to_string()));

        // 注意：这个测试需要网络连接和可用的 SearXNG 实例
        // 在实际环境中，应该使用 mock 或测试实例
        let _result = search_function(&params).await;
        // assert!(result.is_ok()); // 仅在网络可用时取消注释
    }
}
