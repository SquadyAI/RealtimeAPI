//! 百度 TTS 配置
//!
//! ## 环境变量
//!
//! 仅支持 API Key + Secret Key（会自动换取 access_token 用于 WebSocket 请求）。
//!
//! | 环境变量 | 说明 |
//! | --- | --- |
//! | `BAIDU_TTS_API_KEY` | API Key |
//! | `BAIDU_TTS_SECRET_KEY` | Secret Key |
//!
//! ### 通用配置
//!
//! | 环境变量 | 说明 |
//! | --- | --- |
//! | `BAIDU_TTS_PER` | 可选，默认发音人 ID（通常由请求/voice_id 动态覆盖）|

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

use super::types::SystemStartPayload;

/// OAuth 令牌端点
pub const OAUTH_TOKEN_URL: &str = "https://aip.baidubce.com/oauth/2.0/token";

/// 默认 WebSocket 端点
pub const DEFAULT_ENDPOINT: &str = "wss://aip.baidubce.com/ws/2.0/speech/publiccloudspeech/v1/tts";

/// 默认语速
pub const DEFAULT_SPD: u8 = 6;

/// 默认音调
pub const DEFAULT_PIT: u8 = 6;

/// 默认音量
pub const DEFAULT_VOL: u8 = 5;

/// 固定音频格式：PCM-16k
pub const FIXED_AUE_PCM16K: u8 = 4;

/// 固定采样率：16k
pub const FIXED_SAMPLE_RATE_16K: u32 = 16000;

/// Token 刷新提前量（秒）- 在过期前 5 分钟刷新
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

/// OAuth 响应结构
#[derive(Debug, Deserialize)]
struct OAuthResponse {
    access_token: String,
    /// Token 有效期（秒），通常为 2592000（30天）
    expires_in: u64,
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[allow(dead_code)]
    scope: Option<String>,
    #[allow(dead_code)]
    session_key: Option<String>,
    #[allow(dead_code)]
    session_secret: Option<String>,
}

/// OAuth 错误响应
#[derive(Debug, Deserialize)]
struct OAuthError {
    error: String,
    error_description: String,
}

/// 缓存的 Token 信息
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }

    fn needs_refresh(&self) -> bool {
        Instant::now() + Duration::from_secs(TOKEN_REFRESH_MARGIN_SECS) >= self.expires_at
    }
}

/// 全局 Token 缓存
static TOKEN_CACHE: std::sync::OnceLock<Arc<RwLock<Option<CachedToken>>>> = std::sync::OnceLock::new();

fn get_token_cache() -> &'static Arc<RwLock<Option<CachedToken>>> {
    TOKEN_CACHE.get_or_init(|| Arc::new(RwLock::new(None)))
}

/// 百度 TTS 配置
#[derive(Debug, Clone)]
pub struct BaiduTtsConfig {
    /// API Key（必填）
    pub api_key: String,
    /// Secret Key（必填）
    pub secret_key: String,
    /// 发音人 ID
    pub per: Option<String>,
    /// 语速 0-15
    pub spd: u8,
    /// 音调 0-15
    pub pit: u8,
    /// 音量 0-15
    pub vol: u8,
}

impl BaiduTtsConfig {
    /// 从环境变量构建配置（仅支持 API Key + Secret Key）
    pub fn from_env() -> Result<Self> {
        let api_key = env::var("BAIDU_TTS_API_KEY").context("缺少环境变量 BAIDU_TTS_API_KEY")?;
        let secret_key = env::var("BAIDU_TTS_SECRET_KEY").context("缺少环境变量 BAIDU_TTS_SECRET_KEY")?;

        // per 可由每次请求覆盖；因此这里不强制要求设置 BAIDU_TTS_PER
        // 若最终未提供 per（env 或 request），会在实际发起 synthesize 时返回错误。
        let per = env::var("BAIDU_TTS_PER").ok();

        Ok(Self { api_key, secret_key, per, spd: DEFAULT_SPD, pit: DEFAULT_PIT, vol: DEFAULT_VOL })
    }

    /// 设置百度侧 prosody 参数（0-15），由上层（如 `main.rs` / 运行时配置）注入。
    ///
    /// 注意：MiniMax 与 Baidu 的参数体系不同，必须分别维护。
    pub fn with_prosody(mut self, spd: u8, pit: u8, vol: u8) -> Self {
        self.spd = spd.min(15);
        self.pit = pit.min(15);
        self.vol = vol.min(15);
        self
    }

    /// 获取 access_token（自动用 API Key + Secret Key 获取/刷新）
    pub async fn get_access_token(&self) -> Result<String> {
        Self::fetch_or_refresh_token(&self.api_key, &self.secret_key).await
    }

    /// 从缓存获取或刷新 token
    async fn fetch_or_refresh_token(api_key: &str, secret_key: &str) -> Result<String> {
        let cache = get_token_cache();

        // 先尝试从缓存读取
        {
            let cached = cache.read().await;
            if let Some(token) = cached.as_ref() {
                if !token.is_expired() {
                    if token.needs_refresh() {
                        debug!("百度 TTS: Token 即将过期，后台刷新");
                        // 在后台刷新，但先返回当前 token
                        let api_key = api_key.to_string();
                        let secret_key = secret_key.to_string();
                        tokio::spawn(async move {
                            let _ = Self::refresh_token_internal(&api_key, &secret_key).await;
                        });
                    }
                    return Ok(token.access_token.clone());
                }
            }
        }

        // 缓存无效，需要获取新 token
        Self::refresh_token_internal(api_key, secret_key).await
    }

    /// 内部方法：刷新 token
    async fn refresh_token_internal(api_key: &str, secret_key: &str) -> Result<String> {
        let cache = get_token_cache();

        info!("百度 TTS: 正在获取新的 Access Token...");

        let url = format!(
            "{}?client_id={}&client_secret={}&grant_type=client_credentials",
            OAUTH_TOKEN_URL, api_key, secret_key
        );

        let client = reqwest::Client::new();
        let response = client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .body("")
            .send()
            .await
            .context("百度 OAuth 请求失败")?;

        let status = response.status();
        let body = response.text().await.context("读取 OAuth 响应失败")?;

        if !status.is_success() {
            // 尝试解析错误响应
            if let Ok(err) = serde_json::from_str::<OAuthError>(&body) {
                return Err(anyhow!("百度 OAuth 错误: {} - {}", err.error, err.error_description));
            }
            return Err(anyhow!("百度 OAuth 请求失败: status={}, body={}", status, body));
        }

        let oauth_resp: OAuthResponse = serde_json::from_str(&body).context("解析 OAuth 响应失败")?;

        let expires_at = Instant::now() + Duration::from_secs(oauth_resp.expires_in);
        let access_token = oauth_resp.access_token.clone();

        // 更新缓存
        {
            let mut cached = cache.write().await;
            *cached = Some(CachedToken { access_token: access_token.clone(), expires_at });
        }

        info!("百度 TTS: Access Token 获取成功，有效期 {} 秒", oauth_resp.expires_in);

        Ok(access_token)
    }

    /// 构建 WebSocket 连接 URL（包含 query 参数）
    ///
    /// 注意：此方法需要 access_token，对于 ApiKeySecret 认证方式，
    /// 请先调用 `get_access_token()` 获取 token
    /// 构建带 per 的 WebSocket URL（百度新模块：要求显式提供 per）
    pub fn build_ws_url_with_per(&self, access_token: &str, per: &str) -> String {
        format!("{}?access_token={}&per={}", DEFAULT_ENDPOINT, access_token, per)
    }

    /// 构建 system.start 请求的 payload
    pub fn build_start_payload(&self) -> SystemStartPayload {
        SystemStartPayload {
            spd: Some(self.spd),
            pit: Some(self.pit),
            vol: Some(self.vol),
            aue: Some(FIXED_AUE_PCM16K),
            audio_ctrl: Some(format!(r#"{{"sampling_rate":{}}}"#, FIXED_SAMPLE_RATE_16K)),
        }
    }

    /// 清除 token 缓存（用于测试或强制刷新）
    pub async fn clear_token_cache() {
        let cache = get_token_cache();
        let mut cached = cache.write().await;
        *cached = None;
        debug!("百度 TTS: Token 缓存已清除");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 辅助函数：创建测试用的配置（使用 API Key + Secret Key）
    fn test_config_with_keys(api_key: &str, secret_key: &str) -> BaiduTtsConfig {
        BaiduTtsConfig {
            api_key: api_key.to_string(),
            secret_key: secret_key.to_string(),
            per: Some("4189".to_string()),
            spd: 6,
            pit: 6,
            vol: 5,
        }
    }

    // ============== 配置构建测试 ==============

    #[test]
    fn test_build_ws_url() {
        let config = test_config_with_keys("k", "s");
        let url = config.build_ws_url_with_per("test_token", "4189");
        assert!(url.contains("access_token=test_token"));
        assert!(url.contains("per=4189"));
        assert!(url.starts_with("wss://"));
    }

    #[test]
    fn test_build_ws_url_custom_endpoint() {
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("1234".to_string()),
            spd: 6,
            pit: 6,
            vol: 5,
        };

        let url = config.build_ws_url_with_per("my_token", "1234");
        // endpoint 现在固定为 DEFAULT_ENDPOINT
        assert!(url.starts_with(DEFAULT_ENDPOINT));
        assert!(url.contains("access_token=my_token"));
        assert!(url.contains("per=1234"));
    }

    #[test]
    fn test_build_start_payload() {
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("4189".to_string()),
            spd: 7,
            pit: 8,
            vol: 9,
        };

        let payload = config.build_start_payload();
        assert_eq!(payload.spd, Some(7));
        assert_eq!(payload.pit, Some(8));
        assert_eq!(payload.vol, Some(9));
        assert_eq!(payload.aue, Some(FIXED_AUE_PCM16K));
        assert!(payload.audio_ctrl.as_ref().unwrap().contains("16000"));
    }

    #[test]
    fn test_build_start_payload_all_formats() {
        // Baidu 新模块固定 PCM16k，不支持配置多种 aue
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("4189".to_string()),
            spd: 6,
            pit: 6,
            vol: 5,
        };
        let payload = config.build_start_payload();
        assert_eq!(payload.aue, Some(FIXED_AUE_PCM16K));
    }

    #[test]
    fn test_build_start_payload_boundary_values() {
        // 测试边界值
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("4189".to_string()),
            spd: 0,  // 最小值
            pit: 15, // 最大值
            vol: 15, // 最大值
        };

        let payload = config.build_start_payload();
        assert_eq!(payload.spd, Some(0));
        assert_eq!(payload.pit, Some(15));
        assert_eq!(payload.vol, Some(15));
        // 采样率固定 16k
        assert!(payload.audio_ctrl.as_ref().unwrap().contains("16000"));
    }

    #[test]
    fn test_build_start_payload_json_format() {
        let config = test_config_with_keys("k", "s");
        let payload = config.build_start_payload();

        // 验证 audio_ctrl 是有效的 JSON
        let audio_ctrl = payload.audio_ctrl.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&audio_ctrl).unwrap();
        assert_eq!(parsed["sampling_rate"], FIXED_SAMPLE_RATE_16K);
    }

    // ============== 默认常量测试 ==============

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_SPD, 6);
        assert_eq!(DEFAULT_PIT, 6);
        assert_eq!(DEFAULT_VOL, 5);
        assert_eq!(FIXED_AUE_PCM16K, 4); // PCM-16k
        assert_eq!(FIXED_SAMPLE_RATE_16K, 16000);
        assert!(DEFAULT_ENDPOINT.starts_with("wss://"));
        assert!(DEFAULT_ENDPOINT.contains("baidubce.com"));
        assert!(OAUTH_TOKEN_URL.contains("oauth"));
    }

    // ============== 配置克隆测试 ==============

    #[test]
    fn test_config_clone() {
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("4189".to_string()),
            spd: 10,
            pit: 8,
            vol: 12,
        };

        let cloned = config.clone();
        assert_eq!(cloned.per, config.per);
        assert_eq!(cloned.spd, config.spd);
        assert_eq!(cloned.pit, config.pit);
        assert_eq!(cloned.vol, config.vol);
    }

    #[test]
    fn test_config_clone_with_api_key() {
        let config = BaiduTtsConfig {
            api_key: "test_api_key".to_string(),
            secret_key: "test_secret_key".to_string(),
            per: Some("4189".to_string()),
            spd: 6,
            pit: 6,
            vol: 5,
        };

        let cloned = config.clone();
        assert_eq!(cloned.api_key, config.api_key);
        assert_eq!(cloned.secret_key, config.secret_key);
    }

    // ============== Debug 输出测试 ==============

    #[test]
    fn test_config_debug() {
        let config = test_config_with_keys("k", "s");
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("BaiduTtsConfig"));
        assert!(debug_str.contains("per"));
        assert!(debug_str.contains("4189"));
    }

    // ============== Token 缓存测试 ==============

    #[test]
    fn test_cached_token_expiry() {
        let token = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now() - Duration::from_secs(1), // 已过期
        };
        assert!(token.is_expired());
    }

    #[test]
    fn test_cached_token_not_expired() {
        let token = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now() + Duration::from_secs(3600), // 1小时后过期
        };
        assert!(!token.is_expired());
    }

    #[test]
    fn test_cached_token_needs_refresh() {
        // 即将在 4 分钟后过期（小于 5 分钟的刷新提前量）
        let token = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now() + Duration::from_secs(240),
        };
        assert!(token.needs_refresh());
    }

    #[test]
    fn test_cached_token_no_refresh_needed() {
        // 1 小时后过期，不需要刷新
        let token = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now() + Duration::from_secs(3600),
        };
        assert!(!token.needs_refresh());
    }

    // ============== OAuth 响应解析测试 ==============

    #[test]
    fn test_oauth_response_parsing() {
        let json = r#"{
            "access_token": "test_access_token",
            "expires_in": 2592000,
            "refresh_token": "test_refresh_token",
            "scope": "audio_tts_post",
            "session_key": "test_session_key",
            "session_secret": "test_session_secret"
        }"#;

        let resp: OAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "test_access_token");
        assert_eq!(resp.expires_in, 2592000);
    }

    #[test]
    fn test_oauth_response_minimal() {
        // 只有必需字段
        let json = r#"{
            "access_token": "minimal_token",
            "expires_in": 3600
        }"#;

        let resp: OAuthResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.access_token, "minimal_token");
        assert_eq!(resp.expires_in, 3600);
        assert!(resp.refresh_token.is_none());
    }

    #[test]
    fn test_oauth_error_parsing() {
        let json = r#"{
            "error": "invalid_client",
            "error_description": "Client authentication failed"
        }"#;

        let err: OAuthError = serde_json::from_str(json).unwrap();
        assert_eq!(err.error, "invalid_client");
        assert_eq!(err.error_description, "Client authentication failed");
    }

    // 注意：get_access_token 会发起真实网络请求，这里不做单元测试覆盖。
}
