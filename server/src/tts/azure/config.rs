//! Azure TTS 配置

use super::types::AzureTtsError;

/// Azure TTS 配置
#[derive(Debug, Clone)]
pub struct AzureTtsConfig {
    /// Azure 语音服务订阅密钥
    pub subscription_key: String,
    /// Azure 区域 (如 eastasia, eastus, westeurope)
    pub region: String,
    /// 输出音频格式
    pub output_format: String,
    /// 默认语速 (如 "+0%", "+20%", "-10%")
    pub rate: Option<String>,
    /// 默认音调 (如 "+0Hz", "+5Hz")
    pub pitch: Option<String>,
}

impl AzureTtsConfig {
    /// 从环境变量加载配置
    pub fn from_env() -> Result<Self, AzureTtsError> {
        let subscription_key = std::env::var("AZURE_SPEECH_KEY").map_err(|_| AzureTtsError::Config("缺少环境变量 AZURE_SPEECH_KEY".to_string()))?;

        let region = std::env::var("AZURE_SPEECH_REGION").unwrap_or_else(|_| "eastasia".to_string());

        // 使用 24kHz 输出然后重采样到 16kHz，音质更好
        let output_format = std::env::var("AZURE_TTS_OUTPUT_FORMAT").unwrap_or_else(|_| "raw-24khz-16bit-mono-pcm".to_string());

        Ok(Self { subscription_key, region, output_format, rate: None, pitch: None })
    }

    /// 获取 TTS REST API 端点
    pub fn tts_endpoint(&self) -> String {
        format!("https://{}.tts.speech.microsoft.com/cognitiveservices/v1", self.region)
    }
}

impl Default for AzureTtsConfig {
    fn default() -> Self {
        Self {
            subscription_key: String::new(),
            region: "eastasia".to_string(),
            output_format: "raw-24khz-16bit-mono-pcm".to_string(),
            rate: None,
            pitch: None,
        }
    }
}
