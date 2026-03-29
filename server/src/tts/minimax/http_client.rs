//! MiniMax TTS HTTP 客户端
//!
//! 支持通过 REST API (`/v1/t2a_v2`) 调用 MiniMax 语音合成。该客户端主要用于在无法使用
//! WebSocket 或希望以简单请求/响应模式工作的场景。
//!
//! # 注意事项
//! - 当前实现会在收到完整 HTTP 响应后一次性解析为音频块，再异步逐块产出。
//!   MiniMax 的 `stream=true` 响应会以 JSON 数组形式返回多个块（每块包含十六进制编码
//!   的音频数据以及 `status` 字段）。
//! - 为了与 WebSocket 客户端保持语义一致，方法会在所有音频块之后补发一个
//!   `sequence_id = u64::MAX`、`is_final = true` 的空块，用于提示上层任务彻底完成。
//!
//! ## 快速示例
//! ```no_run
//! use realtime::tts::minimax::{
//!     MiniMaxConfig, MiniMaxHttpTtsClient, VoiceSetting, AudioSetting, PronunciationDict,
//! };
//!
//! # #[tokio::main]
//! # async fn main() -> anyhow::Result<()> {
//! let config = MiniMaxConfig::default();
//! let http_client = MiniMaxHttpTtsClient::new(config);
//! use futures_util::StreamExt;
//!
//! // 语音设置与 WebSocket 版本一致，只需提供虚拟 voice_id
//! let mut voice_setting = VoiceSetting::default();
//! voice_setting.voice_id = Some("zh_female_wanwanxiaohe_moon_bigtts".to_string());
//!
//! let audio_stream = http_client
//!     .synthesize_text(
//!         "zh_female_wanwanxiaohe_moon_bigtts",
//!         "今天是不是很开心呀，当然了！",
//!         Some(voice_setting),
//!         Some(AudioSetting::default()),
//!         None,
//!         None,
//!         None,
//!         MiniMaxHttpOptions::default(),
//!     )
//!     .await?;
//!
//! tokio::pin!(audio_stream);
//! while let Some(chunk) = audio_stream.next().await {
//!     let chunk = chunk?;
//!     if chunk.is_final {
//!         break;
//!     }
//!     // 处理 chunk.data 中的音频字节（十六进制已解码）
//! }
//! # Ok(())
//! # }
//! ```

use super::normalize_minimax_lang;
use super::types::{BaseResponse, ExtraInfo};
use super::voice_library::global_voice_library;
use super::{AudioChunk, AudioSetting, MiniMaxConfig, MiniMaxError, PronunciationDict, TimbreWeight, VoiceSetting};
use async_stream::try_stream;
use futures_util::Stream;
use once_cell::sync::OnceCell;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Deserializer;
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, info, warn};

/// HTTP 调用的可选项
#[derive(Debug, Clone)]
pub struct MiniMaxHttpOptions {
    /// 是否启用服务端流式响应。MiniMax 当前默认返回 JSON 数组，
    /// 但如果未来开启真正的流式 SSE，可通过此开关控制。
    pub stream: bool,
    /// 是否开启字幕，对应 `subtitle_enable`
    pub subtitle_enable: Option<bool>,
    /// 输出格式（`hex` 或 `url`），对应 `output_format`
    pub output_format: Option<String>,
    /// 语音特效设置
    pub voice_modify: Option<serde_json::Value>,
    /// 是否强制添加 AIGC 水印
    pub aigc_watermark: Option<bool>,
    /// 🆕 是否排除最后一个 chunk 的拼接音频（避免重复）
    /// 设置为 true 时，最后一个 chunk 不包含完整拼接音频，只包含增量数据
    pub exclude_aggregated_audio: Option<bool>,
}

impl Default for MiniMaxHttpOptions {
    fn default() -> Self {
        Self {
            stream: true,
            subtitle_enable: None,
            output_format: None,
            voice_modify: None,
            aigc_watermark: None,
            // 🔧 关键修复：默认排除拼接音频，避免重复播放
            exclude_aggregated_audio: Some(true),
        }
    }
}

/// MiniMax HTTP TTS 客户端
#[derive(Clone)]
pub struct MiniMaxHttpTtsClient {
    config: MiniMaxConfig,
    http_client: reqwest::Client,
}

// 共享的 HTTP 客户端以复用底层连接池（可与预热结合）
static SHARED_MINIMAX_HTTP_CLIENT: OnceCell<reqwest::Client> = OnceCell::new();

impl MiniMaxHttpTtsClient {
    /// 创建新的 HTTP 客户端
    pub fn new(config: MiniMaxConfig) -> Self {
        let pool_idle_max = std::env::var("MINIMAX_POOL_MAX_IDLE_PER_HOST")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(128);
        let http2_keepalive_secs = std::env::var("MINIMAX_HTTP2_KEEPALIVE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(30);
        let tcp_keepalive_secs = std::env::var("MINIMAX_TCP_KEEPALIVE_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(55);

        let http_client = SHARED_MINIMAX_HTTP_CLIENT
            .get_or_init(|| {
                reqwest::Client::builder()
                    // 不在客户端上设置全局超时，改由每个请求独立控制
                    .http2_adaptive_window(true)
                    .http2_keep_alive_interval(Some(Duration::from_secs(http2_keepalive_secs)))
                    .http2_keep_alive_while_idle(true)
                    .tcp_keepalive(Some(Duration::from_secs(tcp_keepalive_secs)))
                    .pool_idle_timeout(Some(Duration::from_secs(tcp_keepalive_secs)))
                    .pool_max_idle_per_host(pool_idle_max)
                    .build()
                    .expect("构建 MiniMax HTTP 客户端失败")
            })
            .clone();
        Self { config, http_client }
    }

    /// 访问配置
    pub fn config(&self) -> &MiniMaxConfig {
        &self.config
    }

    /// 通过 HTTP 调用 MiniMax TTS 并返回音频块流。
    ///
    /// `virtual_voice_id` 会被映射到声音库中配置的实际 `voice_id` 与 API key。
    #[allow(clippy::too_many_arguments)]
    pub async fn synthesize_text(
        &self,
        virtual_voice_id: &str,
        text: &str,
        voice_setting: Option<VoiceSetting>,
        audio_setting: Option<AudioSetting>,
        pronunciation_dict: Option<PronunciationDict>,
        timbre_weights: Option<Vec<TimbreWeight>>,
        language_boost: Option<String>,
        options: MiniMaxHttpOptions,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk, MiniMaxError>> + Send + '_>>, MiniMaxError> {
        if text.trim().is_empty() {
            return Err(MiniMaxError::Config("文本内容不能为空".to_string()));
        }

        let (api_key, actual_voice_id) = self
            .config
            .get_voice_from_library(virtual_voice_id)
            .ok_or_else(|| MiniMaxError::Config(format!("声音库未找到虚拟 voice_id: {}", virtual_voice_id)))?;

        let mut voice_setting = voice_setting.unwrap_or_default();
        voice_setting.voice_id = Some(actual_voice_id.clone());

        // 从声音库读取该 voice_id 的自定义语速和声调配置
        if let Some(speed) = global_voice_library().get_speed(virtual_voice_id) {
            voice_setting.speed = Some(speed);
        }
        if let Some(pitch) = global_voice_library().get_pitch(virtual_voice_id) {
            voice_setting.pitch = Some(pitch);
        }

        // 从声音库读取该 voice_id 的模型配置，未配置时使用全局默认模型
        let model = global_voice_library()
            .get_model(virtual_voice_id)
            .unwrap_or_else(|| self.config.model.clone());

        // 从声音库读取该 voice_id 的情绪配置
        if let Some(emotion) = global_voice_library().get_emotion(virtual_voice_id) {
            voice_setting.emotion = Some(emotion);
        }

        // 从声音库读取该 voice_id 的音量配置
        if let Some(vol) = global_voice_library().get_vol(virtual_voice_id) {
            voice_setting.vol = Some(vol);
        }

        // 🔧 构造 stream_options（如果需要）
        let stream_options = if options.exclude_aggregated_audio.is_some() {
            Some(StreamOptions { exclude_aggregated_audio: options.exclude_aggregated_audio })
        } else {
            None
        };

        // 规范化 language_boost 到 MiniMax 允许的取值
        let normalized_language_boost = language_boost.as_deref().map(|s| normalize_minimax_lang(Some(s)));

        let request_body = HttpTtsRequest {
            model,
            text: text.to_string(),
            stream: options.stream,
            voice_setting,
            audio_setting,
            pronunciation_dict,
            timbre_weights,
            language_boost: normalized_language_boost,
            voice_modify: options.voice_modify,
            subtitle_enable: options.subtitle_enable,
            output_format: options.output_format,
            aigc_watermark: options.aigc_watermark,
            stream_options,
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", api_key)).map_err(|err| MiniMaxError::Auth(err.to_string()))?,
        );

        info!(
            "发送 MiniMax HTTP 请求: voice_id={}, model={}, stream={}, exclude_aggregated_audio={:?}, language_boost={:?}",
            actual_voice_id,
            request_body.model,
            request_body.stream,
            request_body.stream_options.as_ref().and_then(|so| so.exclude_aggregated_audio),
            request_body.language_boost
        );

        let response = self
            .http_client
            .post(&self.config.http_url)
            .headers(headers)
            .json(&request_body)
            .timeout(self.config.timeout())
            .send()
            .await?
            .error_for_status()?;

        let status = response.status();
        debug!("MiniMax HTTP 响应状态: {}", status);

        let body_text = response.text().await?;
        let response_items = parse_http_response_items(&body_text)?;

        // 从 voice_library 获取该 voice_id 对应的增益（未配置时默认 0.0）
        let gain_db = global_voice_library().get_gain_db(virtual_voice_id);
        debug!("MiniMax HTTP TTS gain_db for voice_id '{}': {}dB", virtual_voice_id, gain_db);

        let stream = try_stream! {
            let mut sequence_id: u64 = 0;

            for item in response_items {
                if !item.base_resp.is_success() {
                    let message = item.base_resp.error_message();
                    warn!("MiniMax HTTP 音频块返回错误: {}", message);
                    Err(MiniMaxError::Api(message))?;
                }

                if let Some(data) = item.data {
                    let status = data.status.unwrap_or_default();

                    if let Some(audio_hex) = data.audio {
                        let audio_bytes = decode_hex_audio(&audio_hex)?;
                        let is_final = status == 2;
                        debug!("MiniMax HTTP 收到音频块: status={}, bytes={}, final={}", status, audio_bytes.len(), is_final);

                        // 🔧 通过 exclude_aggregated_audio=true，API 已确保不会重复发送拼接音频
                        // 使用从 voice_library 获取的增益值，MiniMax 输出 44100Hz
                        let chunk = AudioChunk::new_with_gain(audio_bytes, sequence_id, is_final, gain_db);
                        sequence_id = sequence_id.saturating_add(1);
                        yield chunk;
                    } else if status == 2 {
                        debug!("MiniMax HTTP 收到最终块但无音频数据，补发空控制块");
                        let chunk = AudioChunk::new_with_gain(Vec::new(), sequence_id, true, gain_db);
                        sequence_id = sequence_id.saturating_add(1);
                        yield chunk;
                    }
                }

                if let Some(extra) = item.extra_info {
                    debug!(
                        "MiniMax HTTP extra_info: duration={}ms, sample_rate={:?}, format={:?}",
                        extra.audio_length.unwrap_or_default(),
                        extra.audio_sample_rate,
                        extra.audio_format
                    );
                }
            }

            // 为了保持与 WS 客户端一致，追加 session 级别的 final 控制块
            debug!("MiniMax HTTP 补发会话级 final 控制块");
            let final_chunk = AudioChunk::new_with_gain(Vec::new(), u64::MAX, true, gain_db);
            yield final_chunk;
        };

        Ok(Box::pin(stream))
    }

    /// 使用 HEAD 预热与服务端的 HTTP 连接，失败时回退 GET
    pub async fn prewarm_connection(&self) -> anyhow::Result<()> {
        let endpoint = self.config.http_url.clone();
        // 尝试从默认voice获取一个API key（若无则不携带Authorization也可完成握手）
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref vid) = self.config.default_voice_id
            && let Some((api_key, _)) = self.config.get_voice_from_library(vid)
            && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", api_key))
        {
            headers.insert("Authorization", value);
        }

        // 🔧 优化：减少 prewarm 超时时间，避免长时间阻塞初始化流程
        // 原先 HEAD 5s + GET 5s = 10s，现改为 HEAD 2s + GET 2s = 4s
        let head_result = self
            .http_client
            .head(endpoint.clone())
            .headers(headers.clone())
            .timeout(Duration::from_secs(2))
            .send()
            .await;

        if head_result.is_ok() {
            info!("🔥 MiniMax HTTP 连接已预热 (HEAD)");
            return Ok(());
        }

        let get_result = self
            .http_client
            .get(endpoint)
            .headers(headers)
            .timeout(Duration::from_secs(2))
            .send()
            .await;

        match get_result {
            Ok(_) => {
                info!("🔥 MiniMax HTTP 连接已预热 (GET fallback)");
                Ok(())
            },
            Err(e) => {
                warn!("⚠️ MiniMax 连接预热失败 (HEAD+GET): {}", e);
                Err(anyhow::anyhow!(e))
            },
        }
    }

    /// 并发预热多条连接
    pub async fn prewarm_connections(&self, count: usize) -> anyhow::Result<()> {
        if count <= 1 {
            return self.prewarm_connection().await;
        }
        let endpoint = self.config.http_url.clone();
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref vid) = self.config.default_voice_id
            && let Some((api_key, _)) = self.config.get_voice_from_library(vid)
            && let Ok(value) = HeaderValue::from_str(&format!("Bearer {}", api_key))
        {
            headers.insert("Authorization", value);
        }

        let mut tasks = Vec::with_capacity(count);
        for _ in 0..count {
            let client = self.http_client.clone();
            let endpoint_cloned = endpoint.clone();
            let headers_cloned = headers.clone();
            // 🔧 优化：减少并发 prewarm 超时时间至 2s
            tasks.push(tokio::spawn(async move {
                if client
                    .head(endpoint_cloned.clone())
                    .headers(headers_cloned.clone())
                    .timeout(Duration::from_secs(2))
                    .send()
                    .await
                    .is_err()
                {
                    let _ = client
                        .get(endpoint_cloned)
                        .headers(headers_cloned)
                        .timeout(Duration::from_secs(2))
                        .send()
                        .await;
                }
            }));
        }
        for t in tasks {
            let _ = t.await;
        }
        info!("🔥 MiniMax 连接并发预热完成");
        Ok(())
    }
}

fn decode_hex_audio(hex_str: &str) -> Result<Vec<u8>, MiniMaxError> {
    hex::decode(hex_str).map_err(|err| MiniMaxError::Other(format!("音频数据hex解码失败: {}", err)))
}

fn parse_http_response_items(body: &str) -> Result<Vec<HttpTtsResponseItem>, MiniMaxError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(MiniMaxError::Other("MiniMax HTTP 响应为空".to_string()));
    }

    if trimmed.starts_with('[') {
        return serde_json::from_str::<Vec<HttpTtsResponseItem>>(trimmed).map_err(|err| MiniMaxError::Other(format!("MiniMax HTTP 响应解析失败: {}", err)));
    }

    if trimmed.starts_with("data:") {
        let mut items: Vec<HttpTtsResponseItem> = Vec::new();
        for block in trimmed.split("\n\n") {
            let mut data_lines: Vec<&str> = Vec::new();
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix("data:") {
                    let payload = rest.trim();
                    if payload.is_empty() || payload == "[DONE]" {
                        continue;
                    }
                    data_lines.push(payload);
                }
            }
            if data_lines.is_empty() {
                continue;
            }
            let payload = data_lines.join("\n");
            match serde_json::from_str::<HttpTtsResponseItem>(&payload) {
                Ok(item) => items.push(item),
                Err(err) => {
                    return Err(MiniMaxError::Other(format!(
                        "MiniMax HTTP SSE 响应解析失败: {}。原始片段前128字节: {}",
                        err,
                        payload.chars().take(128).collect::<String>()
                    )));
                },
            }
        }
        if items.is_empty() {
            return Err(MiniMaxError::Other(format!(
                "MiniMax HTTP SSE 响应为空: {}",
                trimmed.chars().take(128).collect::<String>()
            )));
        }
        return Ok(items);
    }

    let mut items = Vec::new();
    let deserializer = Deserializer::from_str(trimmed).into_iter::<HttpTtsResponseItem>();
    let mut index = 0usize;
    for result in deserializer {
        index += 1;
        match result {
            Ok(item) => items.push(item),
            Err(err) => {
                return Err(MiniMaxError::Other(format!(
                    "MiniMax HTTP 流式响应解析失败 (entry {}): {}。原始片段前128字节: {}",
                    index,
                    err,
                    trimmed.chars().take(128).collect::<String>()
                )));
            },
        }
    }
    if items.is_empty() {
        return Err(MiniMaxError::Other(format!(
            "MiniMax HTTP 响应格式未知: {}",
            trimmed.chars().take(128).collect::<String>()
        )));
    }
    Ok(items)
}

/// 流式选项配置
#[derive(Debug, Serialize)]
struct StreamOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_aggregated_audio: Option<bool>,
}

#[derive(Debug, Serialize)]
struct HttpTtsRequest {
    model: String,
    text: String,
    stream: bool,
    voice_setting: VoiceSetting,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_setting: Option<AudioSetting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pronunciation_dict: Option<PronunciationDict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timbre_weights: Option<Vec<TimbreWeight>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language_boost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_modify: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle_enable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aigc_watermark: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Deserialize)]
struct HttpTtsResponseItem {
    #[serde(default)]
    data: Option<HttpTtsResponseData>,
    #[serde(default)]
    extra_info: Option<ExtraInfo>,
    #[serde(rename = "base_resp")]
    base_resp: BaseResponse,
}

#[derive(Debug, Deserialize)]
struct HttpTtsResponseData {
    #[serde(default)]
    audio: Option<String>,
    #[serde(default)]
    status: Option<i32>,
}
