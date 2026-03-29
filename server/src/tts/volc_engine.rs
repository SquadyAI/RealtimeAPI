//! VolcEngine TTS HTTP 流式客户端
//!
//! ## 请求参考（摘自官方 2.1 / 2.2 文档）
//! - 路径：`https://openspeech.bytedance.com/api/v3/tts/unidirectional`
//! - 必需请求头：
//!   - `X-Api-App-Id`：控制台获取的 App ID
//!   - `X-Api-Access-Key`：控制台获取的 Access Token
//!   - `X-Api-Resource-Id`：资源信息 ID（如 `seed-tts-2.0`、`seed-icl-2.0` 等）
//!   - `X-Api-Request-Id`：可选，客户端生成的 UUID
//!   - `Accept: text/event-stream`（实际返回可能是 SSE，也可能是逐行 JSON）
//! - 请求体核心字段：
//!   - `user.uid`：用户标识（字符串）
//!   - `req_params.text` 或 `req_params.ssml`：待合成文本（其一非空）
//!   - `req_params.speaker`：发音人
//!   - `req_params.audio_params.format`：`mp3` / `ogg_opus` / `pcm`
//!   - `req_params.audio_params.sample_rate`：`8000` ~ `48000`
//!   - `req_params.audio_params.bit_rate`：仅 MP3 生效，默认 64k~160k
//!   - 其余如情感、语速、音量、缓存、mix_speaker、context_texts 等均可通过 `additions` 或 `mix_speaker` 字段扩展
//!
//! ## 响应参考（摘自官方 2.3 / 3 文档及官方 Python Demo）
//! - 音频事件：`{"code":0,"message":"","data":"<base64>"}` —— 逐行 JSON 或 SSE `data:` 块，需 base64 解码
//! - 文本事件：`{"code":0,"message":"","data":null,"sentence":{...}}`
//! - 结束事件：`{"code":20000000,"message":"ok","data":null}`
//! - 常见错误：
//!   - `40402003`：文本超限
//!   - `45000000`：音色鉴权失败
//!   - `quota exceeded for types: concurrency`：并发限流
//!   - `55000000`：服务端错误
//!
//! 本模块提供一个轻量封装，便于在管线中以流式方式消费 VolcEngine 返回的音频数据。

use crate::tts::minimax::AudioChunk;
use anyhow::{Context, Result, anyhow};
use async_stream::try_stream;
use base64::Engine as _;
use futures_util::{StreamExt, stream::Stream};
use once_cell::sync::OnceCell;
use reqwest::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::env;
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, info, warn};
use uuid::Uuid;

const DEFAULT_ENDPOINT: &str = "https://openspeech.bytedance.com/api/v3/tts/unidirectional";
#[allow(dead_code)]
const DEFAULT_NAMESPACE: &str = "BidirectionalTTS";
const DEFAULT_AUDIO_FORMAT: &str = "pcm";
const DEFAULT_SAMPLE_RATE: u32 = 44_100;
const DEFAULT_USER_UID: &str = "volc_default_user";

// 共享的 reqwest 客户端，复用底层连接池，便于“预热”后跨会话复用
static SHARED_HTTP_CLIENT: OnceCell<reqwest::Client> = OnceCell::new();

/// 火山引擎 TTS 配置
#[derive(Clone, Debug)]
pub struct VolcEngineConfig {
    pub endpoint: String,
    pub app_id: String,
    pub access_key: String,
    pub resource_id: String,
    pub default_speaker: Option<String>,
    pub default_model: Option<String>,
    pub default_namespace: Option<String>,
    pub default_audio_format: String,
    pub default_sample_rate: u32,
}

impl VolcEngineConfig {
    /// 从环境变量构建配置
    ///
    /// | 环境变量 | 说明 |
    /// | --- | --- |
    /// | `VOLC_APP_ID` | 必填，X-Api-App-Id |
    /// | `VOLC_ACCESS_TOKEN` | 必填，X-Api-Access-Key |
    /// | `VOLC_RESOURCE_ID` | 必填，X-Api-Resource-Id |
    /// | `VOLC_ENDPOINT` | 可选，覆盖默认路径 |
    /// | `VOLC_SPEAKER` | 可选，默认发音人 |
    /// | `VOLC_MODEL` | 可选，默认 req_params.model |
    /// | `VOLC_NAMESPACE` | 可选，默认 namespace |
    /// | `VOLC_AUDIO_FORMAT` | 可选，默认 audio_params.format |
    /// | `VOLC_SAMPLE_RATE` | 可选，默认 audio_params.sample_rate |
    pub fn from_env() -> Result<Self> {
        let app_id = env::var("VOLC_APP_ID").context("缺少环境变量 VOLC_APP_ID")?;
        let access_key = env::var("VOLC_ACCESS_TOKEN").context("缺少环境变量 VOLC_ACCESS_TOKEN")?;
        let resource_id = env::var("VOLC_RESOURCE_ID").context("缺少环境变量 VOLC_RESOURCE_ID")?;

        let endpoint = env::var("VOLC_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string());
        let default_speaker = env::var("VOLC_SPEAKER").ok();
        let default_model = env::var("VOLC_MODEL").ok();
        let default_namespace = env::var("VOLC_NAMESPACE").ok();
        let default_audio_format = env::var("VOLC_AUDIO_FORMAT").unwrap_or_else(|_| DEFAULT_AUDIO_FORMAT.to_string());

        let default_sample_rate = env::var("VOLC_SAMPLE_RATE")
            .ok()
            .and_then(|raw| raw.parse::<u32>().ok())
            .unwrap_or(DEFAULT_SAMPLE_RATE);

        Ok(Self {
            endpoint,
            app_id,
            access_key,
            resource_id,
            default_speaker,
            default_model,
            default_namespace,
            default_audio_format,
            default_sample_rate,
        })
    }

    fn build_headers(&self) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        headers.insert("Connection", HeaderValue::from_static("keep-alive"));
        headers.insert(
            "X-Api-App-Id",
            HeaderValue::from_str(&self.app_id).context("设置 X-Api-App-Id 失败")?,
        );
        headers.insert(
            "X-Api-Access-Key",
            HeaderValue::from_str(&self.access_key).context("设置 X-Api-Access-Key 失败")?,
        );
        headers.insert(
            "X-Api-Resource-Id",
            HeaderValue::from_str(&self.resource_id).context("设置 X-Api-Resource-Id 失败")?,
        );
        // 请求 ID 为可选，但生成一个有助于排障
        headers.insert(
            "X-Api-Request-Id",
            HeaderValue::from_str(&Uuid::new_v4().to_string()).context("设置 X-Api-Request-Id 失败")?,
        );
        Ok(headers)
    }
}

/// 高层请求参数
#[derive(Debug, Clone)]
pub struct VolcEngineRequest {
    pub text: Option<String>,
    pub ssml: Option<String>,
    pub speaker: Option<String>,
    pub user_uid: Option<String>,
    pub model: Option<String>,
    pub namespace: Option<String>,
    pub audio_format: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_rate: Option<u32>,
    pub emotion: Option<String>,
    pub emotion_scale: Option<u8>,
    pub speech_rate: Option<i32>,
    pub loudness_rate: Option<i32>,
    pub enable_timestamp: Option<bool>,
    pub additions: Option<serde_json::Value>,
    pub mix_speaker: Option<serde_json::Value>,
    /// 语言类型：cn（中文）、en（英文）等，用于控制数字/日期的朗读方式
    pub language: Option<String>,
}

impl VolcEngineRequest {
    pub fn from_text<T: Into<String>>(text: T) -> Self {
        Self {
            text: Some(text.into()),
            ssml: None,
            speaker: None,
            user_uid: None,
            model: None,
            namespace: None,
            audio_format: None,
            sample_rate: None,
            bit_rate: None,
            emotion: None,
            emotion_scale: None,
            speech_rate: None,
            loudness_rate: None,
            enable_timestamp: None,
            additions: None,
            mix_speaker: None,
            language: None,
        }
    }

    fn into_body(self, config: &VolcEngineConfig) -> Result<VolcEngineRequestBody> {
        let text = self.text;
        let ssml = self.ssml;

        if text.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) && ssml.as_ref().map(|s| s.trim().is_empty()).unwrap_or(true) {
            return Err(anyhow!("VolcEngine 请求需要 text 或 ssml 至少一个非空"));
        }

        let speaker = self
            .speaker
            .or_else(|| config.default_speaker.clone())
            .ok_or_else(|| anyhow!("未配置发音人：请在请求或 VOLC_SPEAKER 中提供"))?;

        let user_uid = self.user_uid.unwrap_or_else(|| DEFAULT_USER_UID.to_string());

        let namespace = self.namespace.or_else(|| config.default_namespace.clone());
        let model = self.model.or_else(|| config.default_model.clone());
        let audio_format = self.audio_format.unwrap_or_else(|| config.default_audio_format.clone());
        let sample_rate = self.sample_rate.unwrap_or(config.default_sample_rate);

        let audio_params = VolcEngineAudioParams {
            format: Some(audio_format),
            sample_rate: Some(sample_rate),
            bit_rate: self.bit_rate,
            emotion: self.emotion,
            emotion_scale: self.emotion_scale,
            speech_rate: self.speech_rate,
            loudness_rate: self.loudness_rate,
            enable_timestamp: self.enable_timestamp,
            language: self.language,
        };

        let req_params = VolcEngineReqParams {
            text,
            ssml,
            model,
            speaker,
            audio_params,
            additions: self.additions,
            mix_speaker: self.mix_speaker,
        };

        Ok(VolcEngineRequestBody { namespace, user: VolcEngineUserSection { uid: user_uid }, req_params })
    }
}

#[derive(Debug, Serialize)]
struct VolcEngineRequestBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
    user: VolcEngineUserSection,
    #[serde(rename = "req_params")]
    req_params: VolcEngineReqParams,
}

#[derive(Debug, Serialize)]
struct VolcEngineUserSection {
    uid: String,
}

#[derive(Debug, Serialize)]
struct VolcEngineReqParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ssml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    speaker: String,
    #[serde(rename = "audio_params")]
    audio_params: VolcEngineAudioParams,
    #[serde(skip_serializing_if = "Option::is_none")]
    additions: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mix_speaker: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct VolcEngineAudioParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bit_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    emotion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    emotion_scale: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speech_rate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loudness_rate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_timestamp: Option<bool>,
    /// 语言类型：cn（中文）、en（英文）等
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
}

/// 流式音频事件
#[derive(Debug, Deserialize)]
struct VolcEngineSsePayload {
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    sentence: Option<VolcEngineSentence>,
    /// 捕获供应商返回的其他非音频字段（如 event/type/task_id/trace_id/progress 等），便于日志诊断
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct VolcEngineSentence {
    #[serde(default)]
    text: String,
}

/// VolcEngine HTTP 流式 TTS 客户端
#[derive(Clone)]
pub struct VolcEngineTtsClient {
    config: VolcEngineConfig,
    http_client: reqwest::Client,
}

impl VolcEngineTtsClient {
    pub fn new(config: VolcEngineConfig) -> Self {
        let http_client_ref = SHARED_HTTP_CLIENT.get_or_init(|| {
            let pool_idle_max = std::env::var("VOLC_POOL_MAX_IDLE_PER_HOST")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(128);
            let http2_keepalive_secs = std::env::var("VOLC_HTTP2_KEEPALIVE_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(30);
            let tcp_keepalive_secs = std::env::var("VOLC_TCP_KEEPALIVE_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(55);
            reqwest::Client::builder()
                .http2_adaptive_window(true)
                .http2_keep_alive_interval(Some(Duration::from_secs(http2_keepalive_secs)))
                .http2_keep_alive_while_idle(true)
                .tcp_keepalive(Some(Duration::from_secs(tcp_keepalive_secs)))
                .pool_idle_timeout(Some(Duration::from_secs(tcp_keepalive_secs)))
                .pool_max_idle_per_host(pool_idle_max)
                .build()
                .expect("构建 reqwest 客户端失败")
        });
        let http_client = http_client_ref.clone();
        Self { config, http_client }
    }

    pub fn from_env() -> Result<Self> {
        let config = VolcEngineConfig::from_env()?;
        Ok(Self::new(config))
    }

    pub fn config(&self) -> &VolcEngineConfig {
        &self.config
    }

    /// 预热与服务端的 HTTP 连接（建立 TCP/TLS/HTTP2 会话并放入连接池）
    ///
    /// 注意：使用 GET 调用 TTS 端点仅用于建立连接，可能返回 405/4xx，但只要网络成功返回即可视为预热成功。
    pub async fn prewarm_connection(&self) -> Result<()> {
        let headers = self.config.build_headers()?;
        let endpoint = self.config.endpoint.clone();
        // 🔧 优化：减少 prewarm 超时时间，避免长时间阻塞初始化流程
        let head_result = self
            .http_client
            .head(endpoint.clone())
            .headers(headers.clone())
            .timeout(Duration::from_secs(2))
            .send()
            .await;

        if head_result.is_ok() {
            info!("🔥 VolcEngine HTTP 连接已预热 (HEAD)");
            return Ok(());
        }

        // 回退到 GET
        let get_result = self
            .http_client
            .get(endpoint)
            .headers(headers)
            .timeout(Duration::from_secs(2))
            .send()
            .await;

        match get_result {
            Ok(_) => {
                info!("🔥 VolcEngine HTTP 连接已预热 (GET fallback)");
                Ok(())
            },
            Err(e) => {
                warn!("⚠️ VolcEngine 连接预热失败 (HEAD+GET): {}", e);
                Err(anyhow!(e))
            },
        }
    }

    /// 并发预热多条连接（受 HTTP/2 复用影响，具体开设的物理连接由对端与协议决定）
    pub async fn prewarm_connections(&self, count: usize) -> Result<()> {
        if count <= 1 {
            return self.prewarm_connection().await;
        }
        let headers = self.config.build_headers()?;
        let endpoint = self.config.endpoint.clone();
        let mut tasks = Vec::with_capacity(count);
        for _ in 0..count {
            let client = self.http_client.clone();
            let endpoint_cloned = endpoint.clone();
            let headers_cloned = headers.clone();
            // 🔧 优化：减少并发 prewarm 超时时间至 2s
            tasks.push(tokio::spawn(async move {
                // 优先 HEAD，如失败回退 GET（只关注握手完成）
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
        info!("🔥 VolcEngine 连接并发预热完成: {} 请求", count);
        Ok(())
    }

    /// 发送单句文本并返回流式音频
    ///
    /// 返回值为 [`AudioChunk`] 流：`code=0` 的音频事件会解码为音频块；
    /// `code=20000000` 会生成一次 `is_final=true` 的收尾块。
    pub fn stream_sentence(&self, request: VolcEngineRequest) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk>> + Send + '_>>> {
        let headers = self.config.build_headers()?;
        let body = request.into_body(&self.config)?;
        let endpoint = self.config.endpoint.clone();
        let client = self.http_client.clone();

        let stream = try_stream! {
            let response = client
                .post(endpoint)
                .headers(headers)
                .json(&body)
                .send()
                .await
                .context("发送 VolcEngine 请求失败")?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                Err(anyhow!("VolcEngine 返回非 2xx 状态: {status}, body={text}"))?;
            } else {
                let mut byte_stream = response.bytes_stream();
                let mut buffer: Vec<u8> = Vec::new();
                let mut sequence_id: u64 = 0;
                let mut final_sent = false;

                while let Some(chunk) = byte_stream.next().await {
                    let chunk = chunk.context("读取 VolcEngine 数据失败")?;
                    if chunk.is_empty() {
                        continue;
                    }
                    buffer.extend_from_slice(&chunk);

                    while let Some(event_raw) = pop_event(&mut buffer) {
                        if event_raw.is_empty() {
                            continue;
                        }
                        if let Some(payload) = parse_payload(&event_raw)? {
                            if let Some(audio_chunk) = dispatch_payload(payload, &mut sequence_id, &mut final_sent)? {
                                let is_final_chunk = audio_chunk.is_final;
                                yield audio_chunk;
                                if is_final_chunk {
                                    break;
                                }
                            }
                        }
                    }

                    if final_sent {
                        break;
                    }
                }

                if !final_sent {
                    if let Some(payload) = parse_remaining_payload(&mut buffer)? {
                        if let Some(audio_chunk) = dispatch_payload(payload, &mut sequence_id, &mut final_sent)? {
                            yield audio_chunk;
                        }
                    }
                }

                if !final_sent {
                    warn!("VolcEngine 流未收到完成事件，主动结束流");
                    Err(anyhow!("VolcEngine 流意外结束（缺少 code=20000000）"))?;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

fn pop_event(buffer: &mut Vec<u8>) -> Option<String> {
    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
        let event_bytes: Vec<u8> = buffer.drain(..pos + 4).collect();
        let event_str = String::from_utf8_lossy(&event_bytes).replace("\r\n", "\n");
        let trimmed = event_str.trim_end_matches('\n').to_string();
        if trimmed.is_empty() {
            return Some(String::new());
        }
        return Some(trimmed);
    }

    if let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
        let event_bytes: Vec<u8> = buffer.drain(..pos + 2).collect();
        let event_str = String::from_utf8_lossy(&event_bytes).into_owned();
        let trimmed = event_str.trim_end_matches('\n').to_string();
        if trimmed.is_empty() {
            return Some(String::new());
        }
        return Some(trimmed);
    }

    if let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
        let event_bytes: Vec<u8> = buffer.drain(..=pos).collect();
        let event_str = String::from_utf8_lossy(&event_bytes).into_owned();
        let trimmed = event_str.trim_matches(|c| c == '\r' || c == '\n').to_string();
        if trimmed.is_empty() {
            return Some(String::new());
        }
        return Some(trimmed);
    }

    None
}

fn parse_payload(raw_event: &str) -> Result<Option<VolcEngineSsePayload>> {
    let trimmed = raw_event.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(payload) = serde_json::from_str::<VolcEngineSsePayload>(trimmed) {
        return Ok(Some(payload));
    }

    if trimmed.starts_with(':') {
        // SSE 注释 / 心跳
        debug!("VolcEngine SSE keepalive/comment: {}", trimmed);
        return Ok(None);
    }

    let mut data_sections: Vec<String> = Vec::new();
    for line in trimmed.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_sections.push(rest.trim_start().to_string());
        }
    }

    if data_sections.is_empty() {
        return Ok(None);
    }

    let data = data_sections.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(Some(VolcEngineSsePayload {
            code: 20000000,
            message: "ok".to_string(),
            data: None,
            sentence: None,
            extra: std::collections::BTreeMap::new(),
        }));
    }

    let payload: VolcEngineSsePayload = serde_json::from_str(&data).with_context(|| format!("解析 VolcEngine JSON 失败: {data}"))?;
    Ok(Some(payload))
}

fn parse_remaining_payload(buffer: &mut Vec<u8>) -> Result<Option<VolcEngineSsePayload>> {
    if buffer.is_empty() {
        return Ok(None);
    }

    let remaining = String::from_utf8_lossy(buffer).to_string();
    buffer.clear();
    let trimmed = remaining.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    parse_payload(trimmed)
}

fn dispatch_payload(payload: VolcEngineSsePayload, sequence_id: &mut u64, final_sent: &mut bool) -> Result<Option<AudioChunk>> {
    match payload.code {
        0 => {
            if let Some(data_b64) = payload.data {
                let audio_bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_b64.trim())
                    .context("解码 VolcEngine 音频数据失败")?;
                // VolcEngine 引擎增益 0dB（无增益），默认采样率 44100Hz
                let chunk = AudioChunk {
                    data: audio_bytes,
                    sequence_id: *sequence_id,
                    is_final: false,
                    sentence_text: None,
                    gain_db: 0.0,
                    sample_rate: DEFAULT_SAMPLE_RATE,
                };
                info!(
                    "收到音频块: seq={}, bytes={}, is_final={}",
                    chunk.sequence_id,
                    chunk.data.len(),
                    chunk.is_final
                );
                *sequence_id = sequence_id.saturating_add(1);
                Ok(Some(chunk))
            } else if let Some(sentence) = payload.sentence {
                info!("VolcEngine 文本事件: sentence='{}', extra={:?}", sentence.text, payload.extra);
                Ok(None)
            } else {
                info!(
                    "VolcEngine 非音频事件: code=0, message='{}', extra={:?}",
                    payload.message, payload.extra
                );
                Ok(None)
            }
        },
        20000000 => {
            *final_sent = true;
            info!("VolcEngine 合成完成: message='{}', extra={:?}", payload.message, payload.extra);
            let final_chunk = AudioChunk {
                data: Vec::new(),
                sequence_id: u64::MAX,
                is_final: true,
                sentence_text: None,
                gain_db: 0.0,
                sample_rate: DEFAULT_SAMPLE_RATE,
            };
            Ok(Some(final_chunk))
        },
        code => {
            let message = if payload.message.is_empty() {
                "VolcEngine 返回错误".to_string()
            } else {
                payload.message
            };
            Err(anyhow!(
                "VolcEngine 流错误: code={code}, message={message}, extra={:?}",
                payload.extra
            ))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pop_event_handles_various_delimiters() {
        let mut buf = b"data: {\"code\":0}\r\n\r\n".to_vec();
        let event = pop_event(&mut buf);
        assert_eq!(event.unwrap(), "data: {\"code\":0}");
        assert!(buf.is_empty());

        let mut buf = b"data: line1\n\ndata: line2\n\n".to_vec();
        let first = pop_event(&mut buf).unwrap();
        assert_eq!(first, "data: line1");
        let second = pop_event(&mut buf).unwrap();
        assert_eq!(second, "data: line2");
        assert!(buf.is_empty());
    }

    #[test]
    fn pop_event_handles_single_newline_json() {
        let mut buf = b"{\"code\":0,\"message\":\"\",\"data\":null}\n".to_vec();
        let event = pop_event(&mut buf);
        assert_eq!(event.unwrap(), "{\"code\":0,\"message\":\"\",\"data\":null}");
        assert!(buf.is_empty());
    }

    #[test]
    fn parse_payload_skips_comments() {
        assert!(parse_payload(":keep-alive").unwrap().is_none());
    }

    #[test]
    fn parse_payload_decodes_done() {
        let payload = parse_payload("data: [DONE]").unwrap().unwrap();
        assert_eq!(payload.code, 20000000);
    }

    #[test]
    fn parse_payload_reads_json_line() {
        let payload = parse_payload("{\"code\":0,\"message\":\"\",\"data\":null}").unwrap().unwrap();
        assert_eq!(payload.code, 0);
    }

    #[test]
    fn parse_remaining_payload_reads_tail_json() {
        let mut buf = b"{\"code\":20000000,\"message\":\"ok\",\"data\":null}".to_vec();
        let payload = parse_remaining_payload(&mut buf).unwrap().unwrap();
        assert_eq!(payload.code, 20000000);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn request_requires_text_or_ssml() {
        let config = VolcEngineConfig {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            app_id: "app".to_string(),
            access_key: "key".to_string(),
            resource_id: "res".to_string(),
            default_speaker: None,
            default_model: None,
            default_namespace: Some(DEFAULT_NAMESPACE.to_string()),
            default_audio_format: DEFAULT_AUDIO_FORMAT.to_string(),
            default_sample_rate: DEFAULT_SAMPLE_RATE,
        };
        let req = VolcEngineRequest {
            text: None,
            ssml: None,
            speaker: None,
            user_uid: None,
            model: None,
            namespace: None,
            audio_format: None,
            sample_rate: None,
            bit_rate: None,
            emotion: None,
            emotion_scale: None,
            speech_rate: None,
            loudness_rate: None,
            enable_timestamp: None,
            additions: None,
            mix_speaker: None,
            language: None,
        };

        let result = req.into_body(&config);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn request_requires_speaker() {
        let mut config = VolcEngineConfig {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            app_id: "app".to_string(),
            access_key: "key".to_string(),
            resource_id: "res".to_string(),
            default_speaker: None,
            default_model: None,
            default_namespace: Some(DEFAULT_NAMESPACE.to_string()),
            default_audio_format: DEFAULT_AUDIO_FORMAT.to_string(),
            default_sample_rate: DEFAULT_SAMPLE_RATE,
        };

        let req = VolcEngineRequest::from_text("你好");
        let error = req.clone().into_body(&config).unwrap_err();
        assert!(error.to_string().contains("发音人"));

        config.default_speaker = Some("zh_female_shuangkuaisisi_moon_bigtts".to_string());
        // 这里只验证构建流时不会报错（实际网络请求需要打桩）
        let req = VolcEngineRequest::from_text("你好");
        assert!(req.into_body(&config).is_ok());
    }
}
