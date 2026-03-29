//! Tavily 搜索客户端
//!
//! 提供 Tavily API 的搜索功能
//! Tavily 是一个专为 AI 代理设计的搜索引擎

use crate::env_utils::{env_bool_or_default, env_or_default, env_string_or_default};
use crate::function_callback::{CallResult, FunctionCallbackError};
use reqwest::Client;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// Tavily 搜索结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TavilyResult {
    /// 结果URL
    pub url: String,
    /// 结果标题
    pub title: String,
    /// 结果内容
    pub content: String,
    /// 相关度分数
    pub score: Option<f64>,
    /// 原始内容
    pub raw_content: Option<String>,
}

/// Tavily 搜索响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TavilyResponse {
    /// 查询字符串
    pub query: String,
    /// 后续问题
    pub follow_up_questions: Option<Vec<String>>,
    /// AI 生成的答案
    pub answer: Option<String>,
    /// 图片列表
    pub images: Vec<String>,
    /// 搜索结果
    pub results: Vec<TavilyResult>,
    /// 响应时间 (秒)
    pub response_time: f32,
    /// 请求 ID
    pub request_id: String,
}

/// Tavily 客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TavilyConfig {
    /// API 基础 URL
    pub base_url: String,
    /// API Token
    pub api_token: String,
    /// 超时时间 (ms)
    pub timeout_ms: u32,
    /// 启用缓存
    pub enable_cache: bool,
    /// 缓存过期时间 (秒)
    pub cache_ttl_seconds: u64,
    /// 默认搜索主题: "general", "finance", "news"
    pub default_topic: String,
    /// 默认搜索深度: "basic", "advanced", "fast"
    pub default_search_depth: String,
    /// 默认返回结果数
    pub default_max_results: usize,
    /// 默认 chunks_per_source
    pub default_chunks_per_source: usize,
    /// 是否包含回答
    pub default_include_answer: bool,
    /// 是否包含原始内容
    pub default_include_raw_content: bool,
    /// 是否包含图片
    pub default_include_images: bool,
    /// 是否包含图片描述
    pub default_include_image_descriptions: bool,
    /// 是否包含 favicon
    pub default_include_favicon: bool,
    /// 包含的域名
    pub default_include_domains: Vec<String>,
    /// 排除的域名
    pub default_exclude_domains: Vec<String>,
    /// 国家代码
    pub default_country: Option<String>,
    /// 是否包含使用统计
    pub default_include_usage: bool,
}

/// 从环境变量读取 Tavily 配置
fn get_tavily_base_url() -> String {
    env_string_or_default("TAVILY_BASE_URL", "https://api.tavily.com")
}

fn get_tavily_api_token() -> String {
    env_string_or_default("TAVILY_API_TOKEN", "")
}

impl Default for TavilyConfig {
    fn default() -> Self {
        Self {
            base_url: get_tavily_base_url(),
            api_token: get_tavily_api_token(),
            timeout_ms: env_or_default("TAVILY_TIMEOUT_MS", 30000), // 30秒
            enable_cache: env_bool_or_default("TAVILY_ENABLE_CACHE", true),
            cache_ttl_seconds: env_or_default("TAVILY_CACHE_TTL_SECONDS", 300), // 5分钟
            default_topic: env_string_or_default("TAVILY_DEFAULT_TOPIC", "general"),
            default_search_depth: env_string_or_default("TAVILY_DEFAULT_SEARCH_DEPTH", "fast"),
            default_max_results: env_or_default("TAVILY_DEFAULT_MAX_RESULTS", 2),
            default_chunks_per_source: env_or_default("TAVILY_DEFAULT_CHUNKS_PER_SOURCE", 3),
            default_include_answer: env_bool_or_default("TAVILY_DEFAULT_INCLUDE_ANSWER", false),
            default_include_raw_content: env_bool_or_default("TAVILY_DEFAULT_INCLUDE_RAW_CONTENT", false),
            default_include_images: env_bool_or_default("TAVILY_DEFAULT_INCLUDE_IMAGES", false),
            default_include_image_descriptions: env_bool_or_default("TAVILY_DEFAULT_INCLUDE_IMAGE_DESCRIPTIONS", false),
            default_include_favicon: env_bool_or_default("TAVILY_DEFAULT_INCLUDE_FAVICON", false),
            default_include_domains: env_string_or_default("TAVILY_DEFAULT_INCLUDE_DOMAINS", "")
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect(),
            default_exclude_domains: env_string_or_default("TAVILY_DEFAULT_EXCLUDE_DOMAINS", "")
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect(),
            default_country: None,
            default_include_usage: false,
        }
    }
}

/// 搜索选项
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TavilySearchOptions {
    /// 搜索主题: "general", "finance", "news"
    pub topic: Option<String>,
    /// 搜索深度
    pub search_depth: Option<String>,
    /// 最大结果数
    pub max_results: Option<usize>,
    /// 每个来源的块数
    pub chunks_per_source: Option<usize>,
    /// 是否包含回答
    pub include_answer: Option<bool>,
    /// 是否包含原始内容
    pub include_raw_content: Option<bool>,
    /// 是否包含图片
    pub include_images: Option<bool>,
    /// 是否包含图片描述
    pub include_image_descriptions: Option<bool>,
    /// 是否包含 favicon
    pub include_favicon: Option<bool>,
    /// 包含的域名
    pub include_domains: Option<Vec<String>>,
    /// 排除的域名
    pub exclude_domains: Option<Vec<String>>,
    /// 国家代码
    pub country: Option<String>,
    /// 开始日期
    pub start_date: Option<String>,
    /// 结束日期
    pub end_date: Option<String>,
    /// 时间范围
    pub time_range: Option<String>,
    /// 是否包含使用统计
    pub include_usage: Option<bool>,
}

/// 缓存条目
#[derive(Debug, Clone)]
struct CacheEntry {
    response: TavilyResponse,
    timestamp: SystemTime,
}

/// Tavily 客户端
pub struct TavilyClient {
    config: TavilyConfig,
    http_client: Client,
    cache: Arc<RwLock<FxHashMap<String, CacheEntry>>>,
}

impl TavilyClient {
    /// 创建新的 Tavily 客户端
    pub async fn new(config: TavilyConfig) -> Result<Self, FunctionCallbackError> {
        if config.api_token.is_empty() {
            return Err(FunctionCallbackError::Config("Tavily API token 未配置".to_string()));
        }

        info!("🔍 初始化 Tavily 客户端: {}", config.base_url);

        // HTTP 客户端配置
        let http_client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms as u64))
            .danger_accept_invalid_certs(false)
            .danger_accept_invalid_hostnames(false)
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .tcp_keepalive(Duration::from_secs(60))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(30))
            .http2_keep_alive_timeout(Duration::from_secs(5))
            .http2_keep_alive_while_idle(true)
            .build()
            .map_err(|e| FunctionCallbackError::Config(format!("HTTP 客户端创建失败: {}", e)))?;

        let client = Self { config, http_client, cache: Arc::new(RwLock::new(FxHashMap::default())) };

        // 验证连接
        client.verify_connection().await?;

        Ok(client)
    }

    /// 验证 API 连接
    async fn verify_connection(&self) -> Result<(), FunctionCallbackError> {
        // 发送一个简单的测试请求来验证连接
        let test_payload = serde_json::json!({
            "query": "test connection",
            "max_results": 1,
            "search_depth": "fast"
        });

        let response = self
            .http_client
            .post(format!("{}/search", self.config.base_url))
            .header("Authorization", format!("Bearer {}", self.config.api_token))
            .header("Content-Type", "application/json")
            .json(&test_payload)
            .send()
            .await
            .map_err(|e| FunctionCallbackError::Network(format!("Tavily API 连接失败: {}", e)))?;

        if !response.status().is_success() {
            return Err(FunctionCallbackError::Network(format!(
                "Tavily API 返回错误状态码: {}",
                response.status()
            )));
        }

        info!("✅ Tavily API 连接验证成功");
        Ok(())
    }

    /// 执行搜索
    pub async fn search(&self, query: &str, options: Option<TavilySearchOptions>) -> Result<TavilyResponse, FunctionCallbackError> {
        let start_time = Instant::now();
        let options = options.unwrap_or_default();

        // 检查缓存
        if self.config.enable_cache {
            if let Some(cached) = self.get_cached_response(query, &options).await? {
                debug!("Tavily 缓存命中: {}", query);
                return Ok(cached);
            }
        }

        // 执行搜索
        let result = self.execute_search(query, &options).await;

        let _response_time = start_time.elapsed().as_secs_f32();

        // 缓存结果
        if let Ok(ref response) = result
            && self.config.enable_cache
        {
            self.cache_response(query, &options, response).await?;
        }

        result
    }

    /// 执行搜索请求
    async fn execute_search(&self, query: &str, options: &TavilySearchOptions) -> Result<TavilyResponse, FunctionCallbackError> {
        // 构建请求体
        let request_body = self.build_search_payload(query, options);

        debug!("执行 Tavily 搜索: {}", query);

        let response = tokio::time::timeout(
            Duration::from_millis(self.config.timeout_ms as u64),
            self.http_client
                .post(format!("{}/search", self.config.base_url))
                .header("Authorization", format!("Bearer {}", self.config.api_token))
                .header("Content-Type", "application/json")
                .json(&request_body)
                .send(),
        )
        .await
        .map_err(|_| FunctionCallbackError::Timeout("Tavily 搜索超时".to_string()))?
        .map_err(|e| FunctionCallbackError::Network(format!("网络请求失败: {}", e)))?;

        let text_body = response
            .text()
            .await
            .map_err(|e| FunctionCallbackError::Other(format!("读取搜索响应失败: {}", e)))?;

        self.parse_search_response(query, text_body)
    }

    /// 构建搜索请求负载
    fn build_search_payload(&self, query: &str, options: &TavilySearchOptions) -> serde_json::Value {
        // 搜索主题
        let topic = options.topic.as_deref().unwrap_or("general");

        // 搜索深度：finance 主题必须使用 basic，其他使用配置默认值
        let search_depth = if topic == "finance" {
            "basic"
        } else {
            self.config.default_search_depth.as_str()
        };

        let mut payload = serde_json::json!({
            "query": query,
            "topic": topic,
            "search_depth": search_depth,
            "max_results": options.max_results.unwrap_or(self.config.default_max_results),
            "chunks_per_source": 1,
            "include_answer": options.include_answer.unwrap_or(self.config.default_include_answer),
            "include_images": false,
            "include_favicon": false,
            "include_usage": false,
        });

        // 可选：时间范围
        if let Some(time_range) = &options.time_range {
            payload["time_range"] = serde_json::json!(time_range);
        }

        payload
    }

    /// 解析搜索响应
    fn parse_search_response(&self, query: &str, text_body: String) -> Result<TavilyResponse, FunctionCallbackError> {
        let data: serde_json::Value = serde_json::from_str(&text_body).map_err(|e| FunctionCallbackError::Other(format!("解析 Tavily 响应失败: {}", e)))?;

        debug!("Tavily 响应: {}", serde_json::to_string_pretty(&data).unwrap_or_default());

        // 解析结果
        let results: Vec<TavilyResult> = if let Some(results_array) = data.get("results").and_then(|r| r.as_array()) {
            results_array
                .iter()
                .map(|item| TavilyResult {
                    url: item.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
                    title: item.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
                    content: item.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    score: item.get("score").and_then(|s| s.as_f64()),
                    raw_content: item.get("raw_content").and_then(|r| r.as_str()).map(|s| s.to_string()),
                })
                .collect()
        } else {
            Vec::new()
        };

        // 解析后续问题
        let follow_up_questions = data
            .get("follow_up_questions")
            .and_then(|f| f.as_array())
            .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|t| t.to_string())).collect());

        // 解析回答
        let answer = data.get("answer").and_then(|a| a.as_str()).map(|s| s.to_string());

        // 解析图片
        let images: Vec<String> = data
            .get("images")
            .and_then(|i| i.as_array())
            .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|t| t.to_string())).collect())
            .unwrap_or_default();

        // 响应时间
        let response_time = data
            .get("response_time")
            .and_then(|t| t.as_f64())
            .map(|f| f as f32)
            .unwrap_or(0.0);

        // 请求 ID
        let request_id = data.get("request_id").and_then(|r| r.as_str()).unwrap_or("").to_string();

        Ok(TavilyResponse {
            query: query.to_string(),
            follow_up_questions,
            answer,
            images,
            results,
            response_time,
            request_id,
        })
    }

    /// 获取缓存的响应
    async fn get_cached_response(&self, query: &str, options: &TavilySearchOptions) -> Result<Option<TavilyResponse>, FunctionCallbackError> {
        let cache_key = self.generate_cache_key(query, options);
        let cache = self.cache.read().await;

        if let Some(entry) = cache.get(&cache_key) {
            let now = SystemTime::now();
            if let Ok(duration) = now.duration_since(entry.timestamp)
                && duration.as_secs() < self.config.cache_ttl_seconds
            {
                return Ok(Some(entry.response.clone()));
            }
        }

        Ok(None)
    }

    /// 缓存响应
    async fn cache_response(&self, query: &str, options: &TavilySearchOptions, response: &TavilyResponse) -> Result<(), FunctionCallbackError> {
        let cache_key = self.generate_cache_key(query, options);
        let entry = CacheEntry { response: response.clone(), timestamp: SystemTime::now() };

        let mut cache = self.cache.write().await;
        cache.insert(cache_key, entry);

        Ok(())
    }

    /// 生成缓存键
    fn generate_cache_key(&self, query: &str, options: &TavilySearchOptions) -> String {
        let mut key = query.to_string();

        if let Some(topic) = &options.topic {
            key.push_str(&format!(":topic={}", topic));
        }
        if let Some(depth) = &options.search_depth {
            key.push_str(&format!(":depth={}", depth));
        }
        if let Some(max) = options.max_results {
            key.push_str(&format!(":max={}", max));
        }
        if let Some(answer) = options.include_answer {
            key.push_str(&format!(":answer={}", answer));
        }

        key
    }
}

/// 全局 Tavily 客户端
static TAVILY_CLIENT: tokio::sync::OnceCell<TavilyClient> = tokio::sync::OnceCell::const_new();

/// 获取或初始化全局 Tavily 客户端
async fn get_or_init_client() -> Result<&'static TavilyClient, FunctionCallbackError> {
    // 已初始化则直接返回
    if let Some(client) = TAVILY_CLIENT.get() {
        return Ok(client);
    }

    // 尝试初始化
    let client = TavilyClient::new(TavilyConfig::default()).await?;

    // 尝试设置全局客户端（可能被其他线程抢先设置）
    let _ = TAVILY_CLIENT.set(client);

    // 返回全局客户端（无论是自己设置的还是其他线程设置的）
    TAVILY_CLIENT
        .get()
        .ok_or_else(|| FunctionCallbackError::Other("Tavily 客户端初始化失败".into()))
}

/// 处理搜索工具调用
pub async fn handle_search_tool(parameters: &FxHashMap<String, serde_json::Value>) -> Result<CallResult, FunctionCallbackError> {
    let query = parameters
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| FunctionCallbackError::InvalidParameters("缺少 'query' 参数".into()))?;

    // 解析可选参数
    let time_range = parameters.get("time_range").and_then(|v| v.as_str()).map(|s| s.to_string());

    let options = TavilySearchOptions { time_range, ..Default::default() };

    // 懒初始化客户端
    let client = get_or_init_client().await?;
    let response = client.search(query, Some(options)).await?;

    info!(
        "🔍 Tavily 搜索完成: {} 条结果, 耗时 {:.2}s",
        response.results.len(),
        response.response_time
    );

    // 只返回必要字段（去掉 url/raw_content）
    let results: Vec<serde_json::Value> = response
        .results
        .iter()
        .map(|r| {
            serde_json::json!({
                "title": r.title,
                "content": r.content,
                "score": r.score
            })
        })
        .collect();

    Ok(CallResult::Success(serde_json::json!({
        "query": response.query,
        "answer": response.answer,
        "results": results
    })))
}
