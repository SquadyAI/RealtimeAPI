use crate::mcp::{GLOBAL_MCP_TOOL_CACHE, HttpMcpClient, McpClient, McpError, McpServerConfig, McpTool};
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// MCP客户端状态
#[derive(Debug, Clone)]
pub enum McpClientState {
    /// 正在连接中
    Connecting,
    /// 已连接，可以使用
    Connected(Arc<McpClient>),
    /// 连接失败
    Failed(String),
    /// 连接已断开
    Disconnected,
}

/// MCP客户端管理器条目
#[derive(Debug)]
struct McpClientEntry {
    /// 客户端状态
    state: McpClientState,
    /// 最后使用时间
    last_used: Instant,
    /// 引用计数（多少个session在使用）
    ref_count: usize,
    // 🆕 工具缓存已移至全局缓存，这里不再需要本地缓存
    // tools_cache: Option<(Vec<McpTool>, Instant)>, // 已移除
}

/// 全局MCP客户端管理器
#[derive(Debug)]
pub struct McpManager {
    /// 客户端池：endpoint -> 客户端条目
    clients: Arc<RwLock<FxHashMap<String, McpClientEntry>>>,
    /// 客户端清理间隔
    cleanup_interval: Duration,
    /// 客户端空闲超时时间
    idle_timeout: Duration,
}

impl McpManager {
    pub fn new() -> Self {
        let manager = Self {
            clients: Arc::new(RwLock::new(FxHashMap::default())),
            cleanup_interval: Duration::from_secs(60), // 每分钟清理一次
            idle_timeout: Duration::from_secs(300),    // 5分钟空闲超时
        };

        // 启动清理任务
        manager.start_cleanup_task();
        manager
    }

    /// 启动定期清理任务
    fn start_cleanup_task(&self) {
        // 捕获必要字段用于异步清理（避免持有 &self 生命周期）
        let clients = self.clients.clone();
        let interval = self.cleanup_interval;
        let idle_timeout = self.idle_timeout;

        tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(interval);
            loop {
                interval_timer.tick().await;
                // 🧹 定期清理长时间空闲且无引用的客户端，避免内存驻留
                // 说明：这里内联实现 cleanup_idle_clients 的逻辑，避免对 &self 的借用
                let mut guard = clients.write().await;
                let now = Instant::now();
                let mut to_remove = Vec::new();
                for (endpoint, entry) in guard.iter() {
                    if entry.ref_count == 0 && now.duration_since(entry.last_used) > idle_timeout {
                        to_remove.push(endpoint.clone());
                    }
                }
                for endpoint in to_remove {
                    if let Some(entry) = guard.remove(&endpoint) {
                        info!("🧹 清理空闲MCP客户端: {}", endpoint);
                        // 断开底层连接（异步进行）
                        if let McpClientState::Connected(client) = entry.state {
                            let client = client.clone();
                            tokio::spawn(async move {
                                if let Err(e) = client.disconnect().await {
                                    warn!("断开MCP客户端连接失败: {}", e);
                                }
                            });
                        }
                    }
                }
            }
        });
    }

    /// 获取或创建MCP客户端
    pub async fn get_client(&self, config: &McpServerConfig) -> Option<Arc<McpClient>> {
        let endpoint = config.endpoint.clone();

        // 先检查现有客户端
        {
            let mut clients = self.clients.write().await;
            if let Some(entry) = clients.get_mut(&endpoint) {
                entry.last_used = Instant::now();
                entry.ref_count += 1;

                match &entry.state {
                    McpClientState::Connected(client) => {
                        debug!("🔗 复用现有MCP客户端: {}", endpoint);
                        return Some(client.clone());
                    },
                    McpClientState::Connecting => {
                        debug!("🔗 MCP客户端正在连接中: {}", endpoint);
                        return None; // 正在连接，不阻塞
                    },
                    McpClientState::Failed(error) => {
                        debug!("🔗 MCP客户端之前连接失败，重试: {} (错误: {})", endpoint, error);
                        // 重置为连接中状态，稍后重试
                        entry.state = McpClientState::Connecting;
                    },
                    McpClientState::Disconnected => {
                        debug!("🔗 MCP客户端已断开，重新连接: {}", endpoint);
                        entry.state = McpClientState::Connecting;
                    },
                }
            } else {
                // 创建新条目，标记为连接中
                clients.insert(
                    endpoint.clone(),
                    McpClientEntry {
                        state: McpClientState::Connecting,
                        last_used: Instant::now(),
                        ref_count: 1,
                        // 已移除本地工具缓存
                    },
                );
                info!("🔗 开始异步连接新的MCP客户端: {}", endpoint);
            }
        }

        // 在后台异步连接
        let clients_clone = self.clients.clone();
        let config_clone = config.clone();
        tokio::spawn(async move {
            Self::connect_client_async(clients_clone, config_clone).await;
        });

        None // 连接中，不阻塞
    }

    /// 异步连接MCP客户端
    async fn connect_client_async(clients: Arc<RwLock<FxHashMap<String, McpClientEntry>>>, config: McpServerConfig) {
        let endpoint = config.endpoint.clone();
        let client = Arc::new(McpClient::new(config.clone()));

        match client.connect().await {
            Ok(()) => {
                info!("✅ MCP客户端连接成功: {}", endpoint);

                // 🆕 预加载工具列表到全局缓存
                match client.get_tools().await {
                    Ok(tools) => {
                        info!("🔧 预加载MCP工具成功: {} 个工具", tools.len());

                        // 缓存到全局缓存中
                        if let Err(e) = GLOBAL_MCP_TOOL_CACHE
                            .cache_tools(&endpoint, tools, Some(config.tool_cache_ttl_secs))
                            .await
                        {
                            warn!("⚠️ 缓存MCP工具失败: {}", e);
                        }
                    },
                    Err(e) => {
                        warn!("⚠️ 预加载MCP工具失败: {}", e);
                    },
                };

                // 更新客户端状态
                let mut clients_guard = clients.write().await;
                if let Some(entry) = clients_guard.get_mut(&endpoint) {
                    entry.state = McpClientState::Connected(client);
                    // 不再存储本地工具缓存
                }
            },
            Err(e) => {
                error!("❌ MCP客户端连接失败: {} - {}", endpoint, e);

                // 更新为失败状态
                let mut clients_guard = clients.write().await;
                if let Some(entry) = clients_guard.get_mut(&endpoint) {
                    entry.state = McpClientState::Failed(e.to_string());
                }
            },
        }
    }

    /// 🆕 获取工具列表（使用全局缓存）- WebSocket MCP
    pub async fn get_tools(&self, config: &McpServerConfig) -> Result<Vec<McpTool>, McpError> {
        let endpoint = &config.endpoint;

        // 优先尝试从全局缓存读取并判断是否过期
        if let Some((cached_tools, is_expired)) = GLOBAL_MCP_TOOL_CACHE.get_tools_with_status(endpoint).await {
            if !is_expired {
                return Ok(cached_tools);
            }

            // 过期：尝试后台刷新；失败则回退缓存
            let client_opt = self.get_client(config).await;
            if let Some(client) = client_opt {
                match client.get_tools().await {
                    Ok(fresh) => {
                        GLOBAL_MCP_TOOL_CACHE
                            .cache_tools(endpoint, fresh.clone(), Some(config.tool_cache_ttl_secs))
                            .await?;
                        info!("🔧 WebSocket MCP 工具已刷新: {} 个工具", fresh.len());
                        return Ok(fresh);
                    },
                    Err(e) => {
                        warn!("⚠️ WebSocket MCP 工具刷新失败，使用过期缓存: {}", e);
                        return Ok(cached_tools);
                    },
                }
            } else {
                warn!("⚠️ MCP客户端未就绪，使用过期缓存");
                return Ok(cached_tools);
            }
        }

        // 缓存未命中：连接并获取
        let client = self
            .get_client(config)
            .await
            .ok_or_else(|| McpError::ConnectionError("MCP客户端未连接或正在连接中".to_string()))?;

        let tools = client.get_tools().await?;
        GLOBAL_MCP_TOOL_CACHE
            .cache_tools(endpoint, tools.clone(), Some(config.tool_cache_ttl_secs))
            .await?;

        // 更新使用时间
        {
            let mut clients = self.clients.write().await;
            if let Some(entry) = clients.get_mut(endpoint) {
                entry.last_used = Instant::now();
            }
        }

        info!("🔧 从WebSocket MCP服务器获取工具: {} 个工具", tools.len());
        Ok(tools)
    }

    /// 🆕 获取HTTP MCP工具列表（使用全局缓存）
    pub async fn get_http_mcp_tools(&self, http_client: &HttpMcpClient) -> Result<Vec<McpTool>, McpError> {
        // 直接使用全局缓存的HTTP MCP工具获取方法
        GLOBAL_MCP_TOOL_CACHE.get_http_mcp_tools(http_client).await
    }

    /// 调用工具
    pub async fn call_tool(&self, config: &McpServerConfig, session_id: &str, name: &str, arguments: Option<serde_json::Value>) -> Result<crate::mcp::McpToolResult, McpError> {
        // 🔧 添加详细的入参日志记录
        info!(
            "🔗 调用 WebSocket MCP 工具: session_id={}, tool_name={}, endpoint={}",
            session_id, name, config.endpoint
        );
        debug!(
            "📝 WebSocket MCP 工具调用参数: session_id={}, tool_name={}, arguments={:?}",
            session_id, name, arguments
        );

        let client = self
            .get_client(config)
            .await
            .ok_or_else(|| McpError::ConnectionError("MCP客户端未连接".to_string()))?;

        // 更新使用时间
        {
            let mut clients = self.clients.write().await;
            if let Some(entry) = clients.get_mut(&config.endpoint) {
                entry.last_used = Instant::now();
            }
        }

        // 包装结果，统一记录成功/失败日志
        match client.call_tool(session_id, name, arguments.clone()).await {
            Ok(result) => {
                info!(
                    "✅ WebSocket MCP 工具调用成功: session_id={}, tool_name={}, is_error={}, content_count={}",
                    session_id,
                    name,
                    result.is_error,
                    result.content.len()
                );
                debug!(
                    "🧾 WebSocket MCP 工具调用返回详情: session_id={}, tool_name={}, result={:?}",
                    session_id, name, result
                );
                Ok(result)
            },
            Err(e) => {
                error!(
                    "❌ WebSocket MCP 工具调用失败: session_id={}, tool_name={}, error={}",
                    session_id, name, e
                );
                if let Some(args) = &arguments {
                    debug!(
                        "🧾 WebSocket MCP 失败调用参数: session_id={}, tool_name={}, arguments={}",
                        session_id,
                        name,
                        serde_json::to_string_pretty(args).unwrap_or_else(|_| format!("{:?}", args))
                    );
                }
                Err(e)
            },
        }
    }

    /// 释放客户端引用
    pub async fn release_client(&self, endpoint: &str) {
        let mut clients = self.clients.write().await;
        if let Some(entry) = clients.get_mut(endpoint) {
            if entry.ref_count > 0 {
                entry.ref_count -= 1;
            }
            debug!("🔗 释放MCP客户端引用: {} (引用计数: {})", endpoint, entry.ref_count);
        }
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> FxHashMap<String, serde_json::Value> {
        let clients = self.clients.read().await;
        let mut stats = FxHashMap::default();

        stats.insert("total_clients".to_string(), serde_json::Value::Number(clients.len().into()));

        let mut connected_count = 0;
        let mut connecting_count = 0;
        let mut failed_count = 0;
        let mut disconnected_count = 0;

        for entry in clients.values() {
            match &entry.state {
                McpClientState::Connected(_) => connected_count += 1,
                McpClientState::Connecting => connecting_count += 1,
                McpClientState::Failed(_) => failed_count += 1,
                McpClientState::Disconnected => disconnected_count += 1,
            }
        }

        stats.insert("connected".to_string(), serde_json::Value::Number(connected_count.into()));
        stats.insert("connecting".to_string(), serde_json::Value::Number(connecting_count.into()));
        stats.insert("failed".to_string(), serde_json::Value::Number(failed_count.into()));
        stats.insert("disconnected".to_string(), serde_json::Value::Number(disconnected_count.into()));

        // 🆕 添加全局缓存统计信息
        let cache_stats = GLOBAL_MCP_TOOL_CACHE.get_cache_stats().await;
        stats.insert(
            "global_tool_cache".to_string(),
            serde_json::Value::Object(cache_stats.into_iter().collect()),
        );

        stats
    }

    /// 🆕 强制刷新工具缓存（优先尝试WebSocket，其次保留原行为）
    pub async fn refresh_tools_cache(&self, endpoint: &str) -> Result<(), McpError> {
        // 尝试找到对应客户端配置并刷新
        let clients = self.clients.read().await;
        if let Some(entry) = clients.get(endpoint)
            && let McpClientState::Connected(client) = &entry.state
        {
            let tools = client.get_tools().await?;
            GLOBAL_MCP_TOOL_CACHE.cache_tools(endpoint, tools, None).await?;
            return Ok(());
        }
        drop(clients);

        // 回退：如果没有活动客户端，仅标记刷新（清理后由下一次访问触发更新）
        GLOBAL_MCP_TOOL_CACHE.refresh_cache(endpoint).await
    }

    /// 🆕 清除所有工具缓存
    pub async fn clear_all_tools_cache(&self) -> Result<(), McpError> {
        GLOBAL_MCP_TOOL_CACHE.clear_all_cache().await
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
