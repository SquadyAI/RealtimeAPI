use crate::env_utils::{env_bool_or_default, env_or_default, env_string_or_default};
use serde::{Deserialize, Serialize};

/// RPC系统总配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    /// 最大并发会话数
    pub max_concurrent_sessions: usize,
    /// WebSocket处理配置
    pub websocket_config: WebSocketConfig,
    /// ASR调度器配置
    pub asr_config: crate::asr::ASRModuleConfig,
}

/// WebSocket处理器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketConfig {
    /// 服务器绑定地址
    pub bind_address: String,
    /// 服务器端口
    pub port: u16,
    /// WebSocket路径
    pub path: String,
    /// 音频块大小（样本数）
    pub chunk_size: usize,
    /// 采样率
    pub sample_rate: u32,
    /// 缓冲区大小
    pub buffer_size: usize,
    /// WebSocket消息超时时间
    pub message_timeout_ms: u64,
    /// 最大重连次数
    pub max_retries: u32,
    /// 启用压缩
    pub enable_compression: bool,
}

/// PacedAudioSender配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PacedSenderConfig {
    pub buffer_size: Option<usize>,
    pub send_rate_multiplier: Option<f32>,
    pub initial_burst_count: Option<usize>,
    pub initial_burst_delay_ms: Option<u64>,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            max_concurrent_sessions: env_or_default("MAX_CONCURRENT_SESSIONS", 100),
            websocket_config: WebSocketConfig::default(),
            asr_config: crate::asr::ASRModuleConfig::default(),
        }
    }
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            bind_address: env_string_or_default("BIND_ADDR", "0.0.0.0")
                .split(':')
                .next()
                .unwrap_or("0.0.0.0")
                .to_string(),
            port: env_string_or_default("BIND_ADDR", "0.0.0.0:8080")
                .split(':')
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            path: env_string_or_default("WS_PATH", "/ws"),
            chunk_size: env_or_default("CHUNK_SIZE", 1024),
            sample_rate: env_or_default("SAMPLE_RATE", 16000),
            buffer_size: env_or_default("BUFFER_SIZE", 8192),
            message_timeout_ms: env_or_default("WS_MESSAGE_TIMEOUT", 5000),
            max_retries: env_or_default("WS_MAX_RETRIES", 3),
            enable_compression: env_bool_or_default("WS_ENABLE_COMPRESSION", true),
        }
    }
}

impl Default for PacedSenderConfig {
    fn default() -> Self {
        Self {
            buffer_size: Some(32),
            send_rate_multiplier: Some(1.05), // 🔧 修复：轻微加速发送，预防客户端underrun
            initial_burst_count: Some(3),     // 🔧 修复：初始发送3个包预填充客户端缓冲区
            initial_burst_delay_ms: Some(5),  // 🔧 初始包间隔5ms，快速建立缓冲
        }
    }
}

impl RpcConfig {
    /// 验证配置的有效性
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_concurrent_sessions == 0 {
            return Err(anyhow::anyhow!("max_concurrent_sessions不能为0"));
        }

        if self.websocket_config.chunk_size == 0 {
            return Err(anyhow::anyhow!("WebSocket chunk_size不能为0"));
        }

        if self.websocket_config.sample_rate == 0 {
            return Err(anyhow::anyhow!("WebSocket sample_rate不能为0"));
        }

        if self.websocket_config.port == 0 {
            return Err(anyhow::anyhow!("WebSocket port不能为0"));
        }

        Ok(())
    }

    /// 获取完整的WebSocket服务器地址
    pub fn websocket_server_url(&self) -> String {
        format!(
            "{}:{}{}",
            self.websocket_config.bind_address, self.websocket_config.port, self.websocket_config.path
        )
    }
}

/// 运行时动态配置更新
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// 当前负载系数 (0.0 - 1.0)
    pub load_factor: f64,
    /// 是否启用自适应调优
    pub adaptive_tuning: bool,
    /// CPU使用率阈值
    pub cpu_threshold: f64,
    /// 内存使用率阈值
    pub memory_threshold: f64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            load_factor: 0.0,
            adaptive_tuning: true,
            cpu_threshold: 0.8,
            memory_threshold: 0.9,
        }
    }
}
