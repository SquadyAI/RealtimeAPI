//! Edge TTS 配置

use sha2::Digest;
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

/// Edge TTS WebSocket URL
pub const EDGE_TTS_WS_URL: &str = "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";

/// 固定的客户端 Token
pub const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";

/// Chromium 版本
#[allow(dead_code)]
pub const CHROMIUM_FULL_VERSION: &str = "143.0.3650.75";

/// SEC_MS_GEC 版本
pub const SEC_MS_GEC_VERSION: &str = "1-143.0.3650.75";

/// Windows 纪元偏移（1601-01-01 到 1970-01-01 的秒数）
const WIN_EPOCH: u64 = 11644473600;

/// 生成 Sec-MS-GEC 令牌
///
/// 基于当前时间（对齐到5分钟）和 TRUSTED_CLIENT_TOKEN 生成 SHA256 哈希
pub fn generate_sec_ms_gec() -> String {
    // 获取当前 Unix 时间戳（秒）
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    // 转换为 Windows 纪元时间
    let ticks = now + WIN_EPOCH;

    // 对齐到最近的 5 分钟（300 秒）
    let ticks = ticks - (ticks % 300);

    // 转换为 100 纳秒间隔（Windows 文件时间格式）
    let ticks = ticks * 10_000_000;

    // 构建要哈希的字符串
    let str_to_hash = format!("{}{}", ticks, TRUSTED_CLIENT_TOKEN);

    // 计算 SHA256 哈希并返回大写的十六进制字符串
    let hash = sha2::Sha256::digest(str_to_hash.as_bytes());
    hex::encode_upper(hash)
}

/// 生成随机 MUID
pub fn generate_muid() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;

    // 生成 16 字节的伪随机数据
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = ((seed.wrapping_mul(1103515245).wrapping_add(12345 + i as u64)) >> 16) as u8;
    }
    hex::encode_upper(bytes)
}

/// Edge TTS 配置
#[derive(Debug, Clone)]
pub struct EdgeTtsConfig {
    /// 默认语音
    pub default_voice: String,
    /// 输出格式
    pub output_format: String,
    /// 连接超时（秒）
    pub connect_timeout_secs: u64,
    /// 语速调整（百分比，如 +20%、-10%）
    pub rate: Option<String>,
    /// 音高调整（Hz，如 +10Hz、-5Hz）
    pub pitch: Option<String>,
    /// 音量调整（百分比，如 +50%、-20%）
    pub volume: Option<String>,
}

impl Default for EdgeTtsConfig {
    fn default() -> Self {
        Self {
            default_voice: env::var("EDGE_TTS_VOICE").unwrap_or_else(|_| "zh-CN-XiaoxiaoNeural".to_string()),
            output_format: "audio-24khz-48kbitrate-mono-mp3".to_string(),
            connect_timeout_secs: 10,
            rate: None,
            pitch: None,
            volume: None,
        }
    }
}

impl EdgeTtsConfig {
    /// 创建新配置
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置语音
    pub fn with_voice(mut self, voice: impl Into<String>) -> Self {
        self.default_voice = voice.into();
        self
    }

    /// 设置语速（百分比，如 "+20%" 或 "-10%"）
    pub fn with_rate(mut self, rate: impl Into<String>) -> Self {
        self.rate = Some(rate.into());
        self
    }

    /// 设置音高（Hz，如 "+10Hz" 或 "-5Hz"）
    pub fn with_pitch(mut self, pitch: impl Into<String>) -> Self {
        self.pitch = Some(pitch.into());
        self
    }

    /// 设置音量（百分比，如 "+50%" 或 "-20%"）
    pub fn with_volume(mut self, volume: impl Into<String>) -> Self {
        self.volume = Some(volume.into());
        self
    }

    /// 从语速倍率转换为百分比字符串
    /// 例如：1.2 -> "+20%"，0.8 -> "-20%"
    pub fn rate_from_multiplier(multiplier: f64) -> String {
        let percent = ((multiplier - 1.0) * 100.0).round() as i32;
        format!("{:+}%", percent)
    }

    /// 从音高半音转换为 Hz 字符串
    /// 例如：1 -> "+1Hz"，-2 -> "-2Hz"
    pub fn pitch_from_semitones(semitones: i32) -> String {
        format!("{:+}Hz", semitones)
    }
}
