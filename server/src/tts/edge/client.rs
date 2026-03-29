//! Edge TTS WebSocket 客户端
//!
//! 通过 WebSocket 连接微软 Edge TTS 服务进行流式语音合成
//! 支持连接池预热，减少合成延迟

use std::pin::Pin;
use std::sync::Arc;

use async_stream::stream;
use chrono::Utc;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, Stream, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::config::{EDGE_TTS_WS_URL, EdgeTtsConfig, SEC_MS_GEC_VERSION, TRUSTED_CLIENT_TOKEN, generate_muid, generate_sec_ms_gec};
use super::mp3_decoder::{Mp3Decoder, resample_to_16k};
use super::types::EdgeTtsError;
use super::voice_mapping::get_voice_for_language;
use crate::tts::minimax::types::{AudioChunk, SAMPLE_RATE_16000};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;

/// 预建立的 WebSocket 连接
struct PrewarmedConnection {
    writer: WsWriter,
    reader: WsReader,
    created_at: std::time::Instant,
}

/// 连接池大小
const POOL_SIZE: usize = 2;

/// 预连接最大存活时间（秒）- 超过后重建
const MAX_CONNECTION_AGE_SECS: u64 = 30;

/// Edge TTS WebSocket 客户端（带连接池）
#[derive(Clone)]
pub struct EdgeTtsClient {
    config: EdgeTtsConfig,
    /// 预连接池
    pool: Arc<Mutex<Vec<PrewarmedConnection>>>,
    /// 是否正在预热
    warming: Arc<std::sync::atomic::AtomicBool>,
}

impl EdgeTtsClient {
    /// 创建新的 Edge TTS 客户端
    pub fn new(config: EdgeTtsConfig) -> Self {
        Self {
            config,
            pool: Arc::new(Mutex::new(Vec::with_capacity(POOL_SIZE))),
            warming: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// 使用默认配置创建客户端
    pub fn with_defaults() -> Self {
        Self::new(EdgeTtsConfig::default())
    }

    /// 预热连接池
    pub async fn prewarm(&self, count: usize) -> Result<(), EdgeTtsError> {
        info!("🔥 Edge TTS 开始预热 {} 个连接", count);

        for i in 0..count {
            match self.create_prewarmed_connection().await {
                Ok(conn) => {
                    let mut pool = self.pool.lock().await;
                    if pool.len() < POOL_SIZE {
                        pool.push(conn);
                        debug!("🔥 Edge TTS 预热连接 {}/{} 成功", i + 1, count);
                    }
                },
                Err(e) => {
                    warn!("⚠️ Edge TTS 预热连接 {}/{} 失败: {}", i + 1, count, e);
                },
            }
        }

        let pool = self.pool.lock().await;
        info!("🔥 Edge TTS 预热完成，连接池大小: {}", pool.len());
        Ok(())
    }

    /// 创建预热的连接（已发送配置消息）
    async fn create_prewarmed_connection(&self) -> Result<PrewarmedConnection, EdgeTtsError> {
        let connection_id = Uuid::new_v4().to_string().replace("-", "");
        let sec_ms_gec = generate_sec_ms_gec();
        let muid = generate_muid();

        let url = format!(
            "{}?TrustedClientToken={}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version={}",
            EDGE_TTS_WS_URL, TRUSTED_CLIENT_TOKEN, connection_id, sec_ms_gec, SEC_MS_GEC_VERSION
        );

        let mut request = url
            .into_client_request()
            .map_err(|e| EdgeTtsError::WebSocket(format!("构建请求失败: {}", e)))?;

        let headers = request.headers_mut();
        headers.insert("Pragma", "no-cache".parse().unwrap());
        headers.insert("Cache-Control", "no-cache".parse().unwrap());
        headers.insert("Origin", "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold".parse().unwrap());
        headers.insert("Accept-Encoding", "gzip, deflate, br, zstd".parse().unwrap());
        headers.insert("Accept-Language", "en-US,en;q=0.9".parse().unwrap());
        headers.insert(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36 Edg/143.0.0.0"
                .parse()
                .unwrap(),
        );
        headers.insert("Cookie", format!("muid={};", muid).parse().unwrap());

        let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| EdgeTtsError::WebSocket(format!("连接失败: {}", e)))?;

        let (mut writer, reader) = ws_stream.split();

        // 发送配置消息
        let config_msg = build_config_message(&self.config.output_format);
        writer
            .send(Message::Text(config_msg.into()))
            .await
            .map_err(|e| EdgeTtsError::WebSocket(format!("发送配置失败: {}", e)))?;

        debug!("Edge TTS 预连接已建立并发送配置");

        Ok(PrewarmedConnection { writer, reader, created_at: std::time::Instant::now() })
    }

    /// 从池中获取连接，如果没有则新建
    async fn get_connection(&self) -> Result<PrewarmedConnection, EdgeTtsError> {
        // 尝试从池中获取
        {
            let mut pool = self.pool.lock().await;
            while let Some(conn) = pool.pop() {
                // 检查连接是否过期
                if conn.created_at.elapsed().as_secs() < MAX_CONNECTION_AGE_SECS {
                    debug!("♻️ Edge TTS 使用预热连接 (age={}ms)", conn.created_at.elapsed().as_millis());

                    // 后台补充连接
                    self.background_refill();

                    return Ok(conn);
                } else {
                    debug!("🗑️ Edge TTS 连接过期，丢弃");
                }
            }
        }

        // 池为空，新建连接
        debug!("📡 Edge TTS 池为空，新建连接");
        let conn = self.create_prewarmed_connection().await?;

        // 后台补充连接
        self.background_refill();

        Ok(conn)
    }

    /// 后台补充连接池
    fn background_refill(&self) {
        // 避免重复预热
        if self.warming.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }

        let client = self.clone();
        tokio::spawn(async move {
            // 补充到池大小
            let current_size = {
                let pool = client.pool.lock().await;
                pool.len()
            };

            for _ in current_size..POOL_SIZE {
                match client.create_prewarmed_connection().await {
                    Ok(conn) => {
                        let mut pool = client.pool.lock().await;
                        if pool.len() < POOL_SIZE {
                            pool.push(conn);
                            debug!("🔥 Edge TTS 后台补充连接成功，池大小: {}", pool.len());
                        }
                    },
                    Err(e) => {
                        warn!("⚠️ Edge TTS 后台补充连接失败: {}", e);
                    },
                }
            }

            client.warming.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }

    /// 根据语言自动选择声音进行合成
    pub async fn synthesize_for_language(&self, text: &str, language: &str) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk, EdgeTtsError>> + Send>>, EdgeTtsError> {
        let voice = get_voice_for_language(language).ok_or_else(|| EdgeTtsError::UnsupportedLanguage(language.to_string()))?;

        self.synthesize(text, Some(voice)).await
    }

    /// 合成文本为音频流
    pub async fn synthesize(&self, text: &str, voice: Option<&str>) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk, EdgeTtsError>> + Send>>, EdgeTtsError> {
        let voice = voice.unwrap_or(&self.config.default_voice).to_string();
        let text = text.to_string();
        let config = self.config.clone();

        let request_id = Uuid::new_v4().to_string().replace("-", "");

        info!(voice = %voice, text_len = text.len(), "Edge TTS 开始合成");

        // 获取连接（从池中或新建）
        let start = std::time::Instant::now();
        let PrewarmedConnection { mut writer, reader, .. } = self.get_connection().await?;
        debug!("Edge TTS 获取连接耗时: {}ms", start.elapsed().as_millis());

        // 发送 SSML 合成请求
        let ssml_msg = build_ssml_message(
            &request_id,
            &voice,
            &text,
            config.rate.as_deref(),
            config.pitch.as_deref(),
            config.volume.as_deref(),
        );
        writer
            .send(Message::Text(ssml_msg.into()))
            .await
            .map_err(|e| EdgeTtsError::WebSocket(format!("发送 SSML 失败: {}", e)))?;

        debug!("Edge TTS 请求已发送");

        // 创建音频流
        let stream = stream! {
            let mut decoder = Mp3Decoder::new();
            let mut sequence_id: u64 = 0;
            let mut read = reader;

            while let Some(msg_result) = read.next().await {
                match msg_result {
                    Ok(Message::Binary(data)) => {
                        debug!("收到二进制消息: {} 字节", data.len());
                        if let Some(audio_data) = extract_audio_data(&data) {
                            debug!("提取到音频数据: {} 字节", audio_data.len());
                            match decoder.decode(&audio_data) {
                                Ok(pcm_data) => {
                                    debug!("解码 PCM 数据: {} 字节", pcm_data.len());
                                    if !pcm_data.is_empty() {
                                        let sample_rate = decoder.sample_rate().unwrap_or(24000);
                                        let resampled = resample_to_16k(&pcm_data, sample_rate);

                                        if !resampled.is_empty() {
                                            yield Ok(AudioChunk::new_with_sample_rate(
                                                resampled,
                                                sequence_id,
                                                false,
                                                SAMPLE_RATE_16000,
                                            ));
                                            sequence_id += 1;
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("MP3 解码警告: {}", e);
                                }
                            }
                        }
                    }
                    Ok(Message::Text(text)) => {
                        debug!("收到文本消息: {} 字节", text.len());
                        if text.contains("Path:turn.end") {
                            debug!("Edge TTS 合成完成");

                            let sample_rate = decoder.sample_rate().unwrap_or(24000);
                            if let Ok(remaining) = decoder.flush() {
                                if !remaining.is_empty() {
                                    let resampled = resample_to_16k(&remaining, sample_rate);
                                    if !resampled.is_empty() {
                                        yield Ok(AudioChunk::new_with_sample_rate(
                                            resampled,
                                            sequence_id,
                                            false,
                                            SAMPLE_RATE_16000,
                                        ));
                                        sequence_id += 1;
                                    }
                                }
                            }

                            yield Ok(AudioChunk::new_with_sample_rate(
                                Vec::new(),
                                sequence_id,
                                true,
                                SAMPLE_RATE_16000,
                            ));
                            break;
                        }
                    }
                    Ok(Message::Close(_)) => {
                        debug!("Edge TTS WebSocket 关闭");
                        break;
                    }
                    Err(e) => {
                        error!("Edge TTS WebSocket 错误: {}", e);
                        yield Err(EdgeTtsError::WebSocket(e.to_string()));
                        break;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// 构建配置消息
fn build_config_message(output_format: &str) -> String {
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
    format!(
        "X-Timestamp:{}\r\n\
         Content-Type:application/json; charset=utf-8\r\n\
         Path:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"true\"}},\"outputFormat\":\"{}\"}}}}}}}}",
        timestamp, output_format
    )
}

/// 构建 SSML 合成请求消息
fn build_ssml_message(request_id: &str, voice: &str, text: &str, rate: Option<&str>, pitch: Option<&str>, volume: Option<&str>) -> String {
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");

    let escaped_text = html_escape::encode_text(text);

    let rate_attr = rate.map(|r| format!(" rate=\"{}\"", r)).unwrap_or_default();
    let pitch_attr = pitch.map(|p| format!(" pitch=\"{}\"", p)).unwrap_or_default();
    let volume_attr = volume.map(|v| format!(" volume=\"{}\"", v)).unwrap_or_default();

    let content = if rate.is_some() || pitch.is_some() || volume.is_some() {
        format!("<prosody{}{}{}>{}</prosody>", rate_attr, pitch_attr, volume_attr, escaped_text)
    } else {
        escaped_text.to_string()
    };

    format!(
        "X-RequestId:{}\r\n\
         Content-Type:application/ssml+xml\r\n\
         X-Timestamp:{}\r\n\
         Path:ssml\r\n\r\n\
         <speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>\
         <voice name='{}'>{}</voice></speak>",
        request_id, timestamp, voice, content
    )
}

/// 从二进制消息中提取音频数据
fn extract_audio_data(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 2 {
        return None;
    }

    let header_marker = b"Path:audio\r\n";
    if let Some(pos) = data.windows(header_marker.len()).position(|w| w == header_marker) {
        let after_header = &data[pos + header_marker.len()..];
        if let Some(body_start) = after_header.windows(2).position(|w| w == b"\r\n") {
            return Some(after_header[body_start + 2..].to_vec());
        }
    }

    let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    if header_len + 2 <= data.len() {
        let header_bytes = &data[2..2 + header_len];
        if let Ok(header_str) = std::str::from_utf8(header_bytes) {
            if header_str.contains("Path:audio") {
                return Some(data[2 + header_len..].to_vec());
            }
        }
    }

    None
}

impl Default for EdgeTtsClient {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_ssml_message() {
        let msg = build_ssml_message("test-id", "zh-CN-XiaoxiaoNeural", "你好", None, None, None);
        assert!(msg.contains("zh-CN-XiaoxiaoNeural"));
        assert!(msg.contains("你好"));
        assert!(msg.contains("Path:ssml"));
    }

    #[test]
    fn test_build_ssml_message_with_prosody() {
        let msg = build_ssml_message("test-id", "zh-CN-XiaoxiaoNeural", "你好", Some("+20%"), Some("+5Hz"), None);
        assert!(msg.contains("prosody"));
        assert!(msg.contains("rate=\"+20%\""));
        assert!(msg.contains("pitch=\"+5Hz\""));
    }

    #[test]
    fn test_extract_audio_data() {
        let mut data = Vec::new();
        data.extend_from_slice(b"X-RequestId:test\r\n");
        data.extend_from_slice(b"Path:audio\r\n");
        data.extend_from_slice(b"\r\n");
        data.extend_from_slice(b"MP3_AUDIO_DATA");

        let audio = extract_audio_data(&data);
        assert!(audio.is_some());
        assert_eq!(audio.unwrap(), b"MP3_AUDIO_DATA");
    }
}
