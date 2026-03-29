//! MiniMax TTS 配置模块
//!
//! 所有 API key 和 voice_id 管理都通过声音库系统完成

use super::voice_library::global_voice_library;
use crate::env_utils;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// MiniMax TTS 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniMaxConfig {
    /// WebSocket端点URL（用于同步语音合成）
    #[serde(default = "MiniMaxConfig::default_ws_url")]
    pub ws_url: String,
    /// HTTP 端点 URL（用于 REST 合成）
    #[serde(default = "MiniMaxConfig::default_http_url")]
    pub http_url: String,
    /// 模型
    pub model: String,
    /// 默认voice_id（虚拟voice_id）
    pub default_voice_id: Option<String>,
    /// 连接超时（秒）
    pub connect_timeout_secs: u64,
    /// 请求超时（秒）
    pub timeout_secs: u64,
}

impl Default for MiniMaxConfig {
    fn default() -> Self {
        Self {
            ws_url: Self::default_ws_url(),
            http_url: Self::default_http_url(),
            model: env_utils::env_string_or_default("MINIMAX_TTS_MODEL", "speech-2.6-turbo"),
            default_voice_id: Some(env_utils::env_string_or_default(
                "MINIMAX_TTS_DEFAULT_VOICE_ID",
                "zh_female_wanwanxiaohe_moon_bigtts",
            )),
            connect_timeout_secs: env_utils::env_or_default("MINIMAX_TTS_CONNECT_TIMEOUT_SECS", 10),
            timeout_secs: env_utils::env_or_default("MINIMAX_TTS_TIMEOUT_SECS", 30),
        }
    }
}

impl MiniMaxConfig {
    /// 默认 WebSocket URL
    pub fn default_ws_url() -> String {
        env_utils::env_string_or_default("MINIMAX_TTS_WS_URL", "wss://api.minimaxi.com/ws/v1/t2a_v2")
    }

    /// 默认 HTTP URL
    pub fn default_http_url() -> String {
        env_utils::env_string_or_default("MINIMAX_TTS_HTTP_URL", "https://api.minimaxi.com/v1/t2a_v2")
    }

    /// 创建新配置
    pub fn new(ws_url: Option<String>, http_url: Option<String>, model: Option<String>, default_voice_id: Option<String>) -> Self {
        Self {
            ws_url: ws_url.unwrap_or_else(|| "wss://api.minimaxi.com/ws/v1/t2a_v2".to_string()),
            http_url: http_url.unwrap_or_else(|| "https://api.minimaxi.com/v1/t2a_v2".to_string()),
            model: model.unwrap_or_else(|| "speech-2.6-turbo".to_string()),
            default_voice_id,
            connect_timeout_secs: 10,
            timeout_secs: 30,
        }
    }

    /// 获取连接超时Duration
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_secs(self.connect_timeout_secs)
    }

    /// 获取请求超时Duration
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    /// 从声音库获取指定voice_id的API key和实际voice_id
    /// 返回: Option<(api_key, actual_voice_id)>
    pub fn get_voice_from_library(&self, virtual_voice_id: &str) -> Option<(String, String)> {
        global_voice_library().get_voice(virtual_voice_id)
    }

    /// 检查声音库是否已配置
    pub fn is_voice_library_configured(&self) -> bool {
        global_voice_library().is_configured()
    }
}
