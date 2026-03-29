//! 位置解析模块
//!
//! 通过调用位置解析API，将可能的经纬度格式或不规范的城市名转换为标准化的城市名

use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// 位置解析API端点
const LOCATION_CLEAN_API: &str = "http://localhost:18000";

/// 全局位置解析缓存：原始location -> 解析后的城市名（LRU + 自然失效风格，仅容量约束）
static LOCATION_CACHE: Lazy<DashMap<String, String>> = Lazy::new(|| DashMap::with_capacity(100_000));

/// 访问时间（近似 LRU）: key -> last_accessed
static LOCATION_LAST_ACCESSED: Lazy<DashMap<String, Instant>> = Lazy::new(|| DashMap::with_capacity(100_000));

/// 缓存容量（默认10万，可通过环境变量 LOCATION_CACHE_CAP 覆盖）
static LOCATION_CACHE_CAPACITY: Lazy<usize> = Lazy::new(|| {
    std::env::var("LOCATION_CACHE_CAP")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100_000)
});

/// 后台清理任务（定期修剪到容量）
static _LOCATION_CLEANUP_BG: Lazy<()> = Lazy::new(|| {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            trim_location_cache_to_capacity();
        }
    });
});

fn mark_access(key: &str) {
    LOCATION_LAST_ACCESSED.insert(key.to_string(), Instant::now());
}

fn trim_location_cache_to_capacity() {
    let cap = *LOCATION_CACHE_CAPACITY;
    let len = LOCATION_CACHE.len();
    if len <= cap {
        return;
    }
    let to_remove = len.saturating_sub(cap);
    // 收集访问时间
    let mut entries: Vec<(String, Instant)> = LOCATION_LAST_ACCESSED.iter().map(|e| (e.key().clone(), *e.value())).collect();
    // 旧的在前（升序）
    entries.sort_by_key(|(_, t)| *t);
    for (key, _) in entries.into_iter().take(to_remove) {
        LOCATION_CACHE.remove(&key);
        LOCATION_LAST_ACCESSED.remove(&key);
        debug!("🧹 LOCATION_CACHE LRU 驱逐: {}", key);
    }
}

/// 解析位置信息
///
/// 此函数会将可能的经纬度格式（度分秒或小数）或不规范的城市名转换为标准化的城市名
///
/// # 支持的格式
/// - 度分秒格式：`114°01'31"E 22°37'11"N`
/// - 小数格式：`39.9042,116.4074`
/// - 城市名：`北京` → `北京市`（标准化）
///
/// # 参数
/// - `location`: 原始位置信息
///
/// # 返回值
/// - `Some(String)`: 解析后的标准城市名
/// - `None`: 解析失败
///
/// # 缓存策略
/// - 使用全局缓存，所有连接共享
/// - 相同 location 只调用一次 API
/// - 缓存容量 10万条，进程生命周期内永久缓存
/// - API失败时返回None，由调用方决定是否使用原始值
pub async fn clean_location(location: &str) -> Option<String> {
    // 启动后台清理任务（Lazy，仅首次调用时启动）
    let _ = &_LOCATION_CLEANUP_BG;

    if location.trim().is_empty() {
        warn!("⚠️ 位置解析: 输入为空字符串");
        return None;
    }

    // 1. 先查全局缓存
    if let Some(cached) = LOCATION_CACHE.get(location) {
        debug!("🎯 位置解析缓存命中: {} -> {}", location, cached.value());
        mark_access(location);
        return Some(cached.value().clone());
    }

    // 2. 缓存未命中，调用API
    info!("🌐 调用位置解析API: location={}", location);

    let api_url = format!("{}/{}", LOCATION_CLEAN_API, urlencoding::encode(location));

    match reqwest::get(&api_url).await {
        Ok(response) if response.status().is_success() => {
            match response.text().await {
                Ok(body) => {
                    // 尝试解析JSON格式: {"city":"城市名"}
                    match serde_json::from_str::<serde_json::Value>(&body) {
                        Ok(json) => {
                            if let Some(city) = json.get("city").and_then(|v| v.as_str()) {
                                let city_trimmed = city.trim();
                                if !city_trimmed.is_empty() {
                                    // 3. 存入全局缓存
                                    LOCATION_CACHE.insert(location.to_string(), city_trimmed.to_string());
                                    mark_access(location);
                                    // 超额时异步修剪，避免阻塞当前调用
                                    if LOCATION_CACHE.len() > *LOCATION_CACHE_CAPACITY {
                                        tokio::spawn(async {
                                            trim_location_cache_to_capacity();
                                        });
                                    }
                                    info!("🌍 位置解析成功并缓存: {} -> {}", location, city_trimmed);
                                    Some(city_trimmed.to_string())
                                } else {
                                    warn!("⚠️ 位置解析API返回空城市名: location={}, response={}", location, body);
                                    None
                                }
                            } else {
                                warn!("⚠️ 位置解析API响应缺少city字段: location={}, response={}", location, body);
                                None
                            }
                        },
                        Err(e) => {
                            warn!(
                                "⚠️ 位置解析API响应JSON解析失败: location={}, error={}, response={}",
                                location, e, body
                            );
                            None
                        },
                    }
                },
                Err(e) => {
                    warn!("⚠️ 位置解析API响应读取失败: location={}, error={}", location, e);
                    None
                },
            }
        },
        Ok(response) => {
            warn!("⚠️ 位置解析API返回错误状态: {}, location={}", response.status(), location);
            None
        },
        Err(e) => {
            warn!("⚠️ 位置解析API调用失败: location={}, error={}", location, e);
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // 需要真实的API才能运行
    async fn test_clean_location() {
        // 测试城市名解析
        let result = clean_location("深圳").await;
        assert!(result.is_some());

        // 测试经纬度解析
        let result = clean_location("114.057868,22.543099").await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_clean_location_empty() {
        let result = clean_location("").await;
        assert!(result.is_none());
    }
}
