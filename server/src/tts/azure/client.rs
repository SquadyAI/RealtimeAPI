//! Azure TTS REST API 客户端
//!
//! 通过 Azure 认知服务语音 API 进行流式语音合成
//! 支持 140+ 语言，600+ 神经网络声音

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use futures_util::Stream;
use reqwest::Client;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

use super::config::AzureTtsConfig;
use super::types::AzureTtsError;
use super::voice_mapping::get_voice_for_language;
use crate::tts::edge::resample_to_16k;
use crate::tts::minimax::types::{AudioChunk, SAMPLE_RATE_16000};

/// Azure TTS 客户端
#[derive(Clone)]
pub struct AzureTtsClient {
    config: AzureTtsConfig,
    http_client: Client,
    /// 连接池状态
    pool_ready: Arc<Mutex<bool>>,
}

impl AzureTtsClient {
    /// 创建新的 Azure TTS 客户端
    pub fn new(config: AzureTtsConfig) -> Self {
        let http_client = Client::builder()
            .pool_max_idle_per_host(4)
            .build()
            .expect("创建 HTTP 客户端失败");

        Self { config, http_client, pool_ready: Arc::new(Mutex::new(false)) }
    }

    /// 从环境变量创建客户端
    pub fn from_env() -> Result<Self, AzureTtsError> {
        let config = AzureTtsConfig::from_env()?;
        Ok(Self::new(config))
    }

    /// 预热连接池
    pub async fn prewarm(&self, _count: usize) -> Result<(), AzureTtsError> {
        info!("🔥 Azure TTS 开始预热连接");

        // Azure REST API 使用 HTTP，预热主要是验证配置
        // 发送一个简单的请求来验证凭证
        let endpoint = self.config.tts_endpoint();

        let test_ssml = build_ssml(
            "zh-CN-XiaoxiaoNeural",
            "测试",
            self.config.rate.as_deref(),
            self.config.pitch.as_deref(),
        );

        let response = self
            .http_client
            .post(&endpoint)
            .header("Ocp-Apim-Subscription-Key", &self.config.subscription_key)
            .header("Content-Type", "application/ssml+xml")
            .header("X-Microsoft-OutputFormat", &self.config.output_format)
            .header("User-Agent", "RealTimeAPI-TTS/1.0")
            .body(test_ssml)
            .send()
            .await
            .map_err(|e| AzureTtsError::Http(format!("预热请求失败: {}", e)))?;

        if response.status().is_success() {
            let mut pool_ready = self.pool_ready.lock().await;
            *pool_ready = true;
            info!("🔥 Azure TTS 预热成功，连接池就绪");
            Ok(())
        } else {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            Err(AzureTtsError::Auth(format!("预热失败: HTTP {} - {}", status, body)))
        }
    }

    /// 根据语言自动选择声音进行合成
    pub async fn synthesize_for_language(&self, text: &str, language: &str) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk, AzureTtsError>> + Send>>, AzureTtsError> {
        let voice = get_voice_for_language(language).ok_or_else(|| AzureTtsError::UnsupportedLanguage(language.to_string()))?;

        self.synthesize(text, Some(voice)).await
    }

    /// 合成文本为音频流
    pub async fn synthesize(&self, text: &str, voice: Option<&str>) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk, AzureTtsError>> + Send>>, AzureTtsError> {
        let voice = voice.unwrap_or("zh-CN-XiaoxiaoNeural").to_string();
        let text = text.to_string();
        let config = self.config.clone();
        let http_client = self.http_client.clone();

        info!(voice = %voice, text_len = text.len(), "Azure TTS 开始合成");

        let endpoint = config.tts_endpoint();
        let ssml = build_ssml(&voice, &text, config.rate.as_deref(), config.pitch.as_deref());

        debug!("Azure TTS SSML: {}", ssml);

        let start = std::time::Instant::now();

        // 发送 HTTP 请求
        let response = http_client
            .post(&endpoint)
            .header("Ocp-Apim-Subscription-Key", &config.subscription_key)
            .header("Content-Type", "application/ssml+xml")
            .header("X-Microsoft-OutputFormat", &config.output_format)
            .header("User-Agent", "RealTimeAPI-TTS/1.0")
            .body(ssml)
            .send()
            .await
            .map_err(|e| AzureTtsError::Http(format!("请求失败: {}", e)))?;

        debug!(
            "Azure TTS 请求发送完成，耗时: {}ms，状态: {}",
            start.elapsed().as_millis(),
            response.status()
        );

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(AzureTtsError::Http(format!("合成失败: HTTP {} - {}", status, body)));
        }

        // 创建流式响应
        let stream = stream! {
            let mut sequence_id: u64 = 0;
            let mut byte_stream = response.bytes_stream();

            // 缓冲区，用于累积小块数据
            // 24kHz 100ms = 24000 * 0.1 * 2 = 4800 bytes
            let mut buffer: Vec<u8> = Vec::new();
            const CHUNK_SIZE_24K: usize = 4800; // 100ms of 24kHz 16bit mono PCM

            use futures_util::StreamExt;

            while let Some(chunk_result) = byte_stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.extend_from_slice(&chunk);

                        // 当缓冲区足够大时，重采样并发送
                        while buffer.len() >= CHUNK_SIZE_24K {
                            let audio_24k: Vec<u8> = buffer.drain(..CHUNK_SIZE_24K).collect();
                            // 24kHz -> 16kHz 重采样
                            let audio_16k = resample_to_16k(&audio_24k, 24000);

                            yield Ok(AudioChunk::new_with_sample_rate(
                                audio_16k,
                                sequence_id,
                                false,
                                SAMPLE_RATE_16000,
                            ));
                            sequence_id += 1;
                        }
                    }
                    Err(e) => {
                        error!("Azure TTS 流错误: {}", e);
                        yield Err(AzureTtsError::Http(e.to_string()));
                        return;
                    }
                }
            }

            // 发送剩余数据
            if !buffer.is_empty() {
                let audio_16k = resample_to_16k(&buffer, 24000);
                yield Ok(AudioChunk::new_with_sample_rate(
                    audio_16k,
                    sequence_id,
                    false,
                    SAMPLE_RATE_16000,
                ));
                sequence_id += 1;
            }

            // 发送结束标记
            yield Ok(AudioChunk::new_with_sample_rate(
                Vec::new(),
                sequence_id,
                true,
                SAMPLE_RATE_16000,
            ));

            debug!("Azure TTS 合成完成，共 {} 个音频块", sequence_id);
        };

        Ok(Box::pin(stream))
    }

    /// 获取配置
    pub fn config(&self) -> &AzureTtsConfig {
        &self.config
    }
}

/// 构建 SSML
fn build_ssml(voice: &str, text: &str, rate: Option<&str>, pitch: Option<&str>) -> String {
    let escaped_text = html_escape::encode_text(text);

    let rate_attr = rate.map(|r| format!(" rate=\"{}\"", r)).unwrap_or_default();
    let pitch_attr = pitch.map(|p| format!(" pitch=\"{}\"", p)).unwrap_or_default();

    let content = if rate.is_some() || pitch.is_some() {
        format!("<prosody{}{}>{}</prosody>", rate_attr, pitch_attr, escaped_text)
    } else {
        escaped_text.to_string()
    };

    // 从声音名称提取语言代码
    let lang = voice.split('-').take(2).collect::<Vec<_>>().join("-");

    format!(
        r#"<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="{}"><voice name="{}">{}</voice></speak>"#,
        lang, voice, content
    )
}

impl Default for AzureTtsClient {
    fn default() -> Self {
        Self::new(AzureTtsConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ssml() {
        let ssml = build_ssml("zh-CN-XiaoxiaoNeural", "你好世界", None, None);
        assert!(ssml.contains("zh-CN-XiaoxiaoNeural"));
        assert!(ssml.contains("你好世界"));
        assert!(ssml.contains("xml:lang=\"zh-CN\""));
    }

    #[test]
    fn test_build_ssml_with_prosody() {
        let ssml = build_ssml("en-US-JennyNeural", "Hello", Some("+20%"), Some("+5Hz"));
        assert!(ssml.contains("prosody"));
        assert!(ssml.contains("rate=\"+20%\""));
        assert!(ssml.contains("pitch=\"+5Hz\""));
    }
}
