//! 百度 TTS WebSocket 流式客户端
//!
//! ## 协议流程
//!
//! 1. WebSocket 连接（URL 包含 access_token 和 per 参数）
//! 2. 发送 system.start 请求，设置合成参数
//! 3. 收到 system.started 响应
//! 4. 发送 text 请求（可多次发送）
//! 5. 接收二进制音频数据
//! 6. 发送 system.finish 请求
//! 7. 收到 system.finished 响应
//! 8. 连接关闭

use crate::tts::minimax::AudioChunk;
use anyhow::{Context, Result, anyhow};
use async_stream::try_stream;
use futures_util::stream::Stream;
use futures_util::{SinkExt, StreamExt};
use std::pin::Pin;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::config::{BaiduTtsConfig, FIXED_SAMPLE_RATE_16K};
use super::types::{BaiduTtsError, BaiduTtsResponse, SystemFinishRequest, SystemStartPayload, SystemStartRequest, TextRequest};

/// 连接超时时间（秒）
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// 消息接收超时时间（秒）
const RECEIVE_TIMEOUT_SECS: u64 = 30;

/// 百度 TTS 合成请求
#[derive(Debug, Clone)]
pub struct BaiduTtsRequest {
    /// 待合成文本
    pub text: String,
    /// 可选：覆盖默认的合成参数
    pub start_payload: Option<SystemStartPayload>,
    /// 可选：覆盖默认 per（发音人）
    pub per: Option<String>,
}

impl BaiduTtsRequest {
    /// 创建新的合成请求
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), start_payload: None, per: None }
    }

    /// 设置自定义合成参数
    pub fn with_payload(mut self, payload: SystemStartPayload) -> Self {
        self.start_payload = Some(payload);
        self
    }

    /// 覆盖发音人（per 参数）
    pub fn with_per(mut self, per: impl Into<String>) -> Self {
        self.per = Some(per.into());
        self
    }
}

/// 百度 TTS WebSocket 客户端
#[derive(Clone)]
pub struct BaiduTtsClient {
    config: BaiduTtsConfig,
}

impl BaiduTtsClient {
    /// 创建新的客户端实例
    pub fn new(config: BaiduTtsConfig) -> Self {
        Self { config }
    }

    /// 从环境变量创建客户端
    pub fn from_env() -> Result<Self> {
        let config = BaiduTtsConfig::from_env()?;
        Ok(Self::new(config))
    }

    /// 获取配置引用
    pub fn config(&self) -> &BaiduTtsConfig {
        &self.config
    }

    /// 执行流式语音合成，返回音频块流
    ///
    /// 返回 [`AudioChunk`] 流，与其他 TTS 提供商保持一致的接口
    pub fn synthesize(&self, request: BaiduTtsRequest) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk>> + Send + '_>>> {
        let config = self.config.clone();
        let start_payload = request.start_payload.unwrap_or_else(|| self.config.build_start_payload());
        let per_override = request.per.clone();
        let text = request.text;

        // 验证文本长度（百度限制 1000 字）
        if text.chars().count() > 1000 {
            return Err(anyhow!(BaiduTtsError::TextTooLong("文本过长，请控制在1000字以内".to_string())));
        }

        let stream = try_stream! {
            // 0. 获取 access_token（如果使用 API Key + Secret Key 认证，会自动获取）
            let access_token = config.get_access_token().await
                .context("获取百度 TTS Access Token 失败")?;

            let per = per_override
                .as_deref()
                .or(config.per.as_deref())
                .filter(|p| !p.trim().is_empty())
                .ok_or_else(|| anyhow!(BaiduTtsError::Config("缺少 per：请设置 BAIDU_TTS_PER 或在请求里指定 per".to_string())))?;

            let ws_url = config.build_ws_url_with_per(&access_token, per);

            // 1. 建立 WebSocket 连接
            info!("🔌 百度 TTS: 正在连接 WebSocket...");
            let (ws_stream, response) = match timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS), connect_async(&ws_url)).await {
                Ok(Ok((stream, resp))) => (stream, resp),
                Ok(Err(e)) => Err(anyhow!(BaiduTtsError::WebSocket(e.to_string())))?,
                Err(_) => Err(anyhow!(BaiduTtsError::Timeout("WebSocket 连接超时".to_string())))?,
            };

            info!("✅ 百度 TTS: WebSocket 连接成功, status={}", response.status());
            let (mut write, mut read) = ws_stream.split();

            // 2. 发送 system.start 请求
            let start_request = SystemStartRequest {
                msg_type: "system.start".to_string(),
                payload: Some(start_payload),
            };
            let start_json = serde_json::to_string(&start_request)
                .context("序列化 system.start 请求失败")?;

            debug!("📤 百度 TTS: 发送 system.start: {}", start_json);
            write.send(Message::Text(start_json.into())).await
                .context("发送 system.start 失败")?;

            // 3. 等待 system.started 响应
            match timeout(Duration::from_secs(RECEIVE_TIMEOUT_SECS), read.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let response: BaiduTtsResponse =
                        serde_json::from_str(&text).context("解析 system.started 响应失败")?;
                    if !response.is_success() {
                        Err(anyhow!(BaiduTtsError::Parameter {
                            code: response.code,
                            message: response.error_message(),
                        }))?;
                    }
                    info!("✅ 百度 TTS: system.started 成功, session_id={:?}", response.session_id());
                }
                Ok(Some(Ok(msg))) => {
                    warn!("⚠️ 百度 TTS: 收到非预期消息类型 (期待 system.started): {:?}", msg);
                    Err(anyhow!(BaiduTtsError::Other("未收到 system.started".to_string())))?;
                }
                Ok(Some(Err(e))) => Err(anyhow!(BaiduTtsError::WebSocket(e.to_string())))?,
                Ok(None) => Err(anyhow!(BaiduTtsError::WebSocket("连接意外关闭".to_string())))?,
                Err(_) => Err(anyhow!(BaiduTtsError::Timeout("等待 system.started 超时".to_string())))?,
            };

            // 4. 发送文本请求
            let text_request = TextRequest::new(&text);
            let text_json = serde_json::to_string(&text_request)
                .context("序列化 text 请求失败")?;

            debug!("📤 百度 TTS: 发送 text: {} 字符", text.chars().count());
            write.send(Message::Text(text_json.into())).await
                .context("发送 text 请求失败")?;

            // 5. 发送 system.finish（严格按协议：告知服务端“没有更多文本了”，避免尾句丢失/卡住）
            let finish_request = SystemFinishRequest::default();
            let finish_json = serde_json::to_string(&finish_request)
                .context("序列化 system.finish 请求失败")?;
            debug!("📤 百度 TTS: 发送 system.finish");
            write.send(Message::Text(finish_json.into())).await
                .context("发送 system.finish 失败")?;

            // 6. 接收音频数据直到 system.finished
            let mut sequence_id: u64 = 0;
            let mut audio_received = false;
            let mut finished_seen = false;

            loop {
                let msg_result = timeout(Duration::from_secs(RECEIVE_TIMEOUT_SECS), read.next()).await;

                match msg_result {
                    Ok(Some(Ok(Message::Binary(data)))) => {
                        // 收到二进制音频数据
                        if !data.is_empty() {
                            audio_received = true;
                            debug!("📥 百度 TTS: 收到音频数据, {} bytes, seq={}", data.len(), sequence_id);

                            let chunk = AudioChunk {
                                data: data.to_vec(),
                                sequence_id,
                                is_final: false,
                                sentence_text: if sequence_id == 0 { Some(text.clone()) } else { None },
                                gain_db: 0.0, // 百度 TTS 无增益
                                sample_rate: FIXED_SAMPLE_RATE_16K, // 百度 TTS 输出 16kHz PCM
                            };

                            sequence_id = sequence_id.saturating_add(1);
                            yield chunk;
                        }
                    }
                    Ok(Some(Ok(Message::Text(text_msg)))) => {
                        // 收到 JSON 响应（可能是错误或结束）
                        debug!("📥 百度 TTS: 收到文本消息: {}", text_msg);

                        if let Ok(response) = serde_json::from_str::<BaiduTtsResponse>(&text_msg) {
                            if response.msg_type == "system.error" {
                                Err(anyhow!(BaiduTtsError::Server {
                                    code: response.code,
                                    message: response.error_message(),
                                }))?;
                            } else if response.msg_type == "system.finished" {
                                info!("✅ 百度 TTS: 合成完成 (system.finished)");
                                finished_seen = true;
                                break;
                            }
                        }
                    }
                    Ok(Some(Ok(Message::Close(_)))) => {
                        info!("📪 百度 TTS: 收到关闭帧");
                        break;
                    }
                    Ok(Some(Ok(_))) => {
                        // 忽略 Ping/Pong 等其他消息
                        continue;
                    }
                    Ok(Some(Err(e))) => {
                        Err(anyhow!(BaiduTtsError::WebSocket(e.to_string())))?;
                    }
                    Ok(None) => {
                        // 连接关闭
                        info!("📪 百度 TTS: WebSocket 连接已关闭");
                        break;
                    }
                    Err(_) => {
                        // 超时：若完全未收到音频，视为失败；否则作为“尾段迟迟未 finished”的兜底退出
                        if !audio_received {
                            Err(anyhow!(BaiduTtsError::Timeout("接收音频超时（未收到任何音频）".to_string())))?;
                        }
                        warn!(
                            "⚠️ 百度 TTS: 接收超时（audio_received={}, finished_seen={}），结束读取循环",
                            audio_received, finished_seen
                        );
                        break;
                    }
                }
            }

            // 7. 发送 final 标记
            info!("✅ 百度 TTS: 合成结束, 共 {} 个音频块 (finished_seen={})", sequence_id, finished_seen);

            let final_chunk = AudioChunk {
                data: Vec::new(),
                sequence_id: u64::MAX,
                is_final: true,
                sentence_text: None,
                gain_db: 0.0,
                sample_rate: FIXED_SAMPLE_RATE_16K,
            };
            yield final_chunk;
        };

        Ok(Box::pin(stream))
    }

    /// 预热连接（建立 WebSocket 连接并立即关闭）
    ///
    /// 用于减少首次合成的延迟，同时也会预热 access_token 缓存
    pub async fn prewarm_connection(&self) -> Result<()> {
        // 获取 access_token（如果使用 API Key + Secret Key，这也会预热 token 缓存）
        let access_token = self
            .config
            .get_access_token()
            .await
            .context("预热连接时获取 Access Token 失败")?;

        let per = match self.config.per.as_deref().filter(|p| !p.trim().is_empty()) {
            Some(p) => p,
            None => {
                // per 现在可由每次请求动态覆盖；预热阶段缺少 per 就直接跳过（不视为失败）
                debug!("🔥 百度 TTS: 预热跳过（未设置 BAIDU_TTS_PER 且预热无请求上下文）");
                return Ok(());
            },
        };

        let ws_url = self.config.build_ws_url_with_per(&access_token, per);

        let connect_result = timeout(Duration::from_secs(CONNECT_TIMEOUT_SECS), connect_async(&ws_url)).await;

        match connect_result {
            Ok(Ok((ws_stream, _))) => {
                let (mut write, _) = ws_stream.split();
                // 发送关闭帧
                let _ = write.close().await;
                info!("🔥 百度 TTS: WebSocket 连接预热完成");
                Ok(())
            },
            Ok(Err(e)) => {
                warn!("⚠️ 百度 TTS: 连接预热失败: {}", e);
                Err(anyhow!(e))
            },
            Err(_) => {
                warn!("⚠️ 百度 TTS: 连接预热超时");
                Err(anyhow!("连接预热超时"))
            },
        }
    }

    /// 并发预热多条连接
    pub async fn prewarm_connections(&self, count: usize) -> Result<()> {
        if count <= 1 {
            return self.prewarm_connection().await;
        }

        let mut tasks = Vec::with_capacity(count);
        for _ in 0..count {
            let client = self.clone();
            tasks.push(tokio::spawn(async move {
                let _ = client.prewarm_connection().await;
            }));
        }

        for task in tasks {
            let _ = task.await;
        }

        info!("🔥 百度 TTS: 连接并发预热完成: {} 请求", count);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 辅助函数：创建测试用的配置
    fn test_config() -> BaiduTtsConfig {
        BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("4189".to_string()),
            spd: 6,
            pit: 6,
            vol: 5,
        }
    }

    // ============== BaiduTtsRequest 测试 ==============

    #[test]
    fn test_request_creation() {
        let request = BaiduTtsRequest::new("测试文本");
        assert_eq!(request.text, "测试文本");
        assert!(request.start_payload.is_none());
        assert!(request.per.is_none());
    }

    #[test]
    fn test_request_creation_from_string() {
        let text = String::from("动态字符串");
        let request = BaiduTtsRequest::new(text);
        assert_eq!(request.text, "动态字符串");
    }

    #[test]
    fn test_request_creation_empty() {
        let request = BaiduTtsRequest::new("");
        assert_eq!(request.text, "");
        assert!(request.start_payload.is_none());
        assert!(request.per.is_none());
    }

    #[test]
    fn test_request_with_payload() {
        let payload = SystemStartPayload { spd: Some(10), pit: Some(8), vol: Some(7), aue: Some(3), audio_ctrl: None };

        let request = BaiduTtsRequest::new("测试").with_payload(payload);
        assert!(request.start_payload.is_some());
        assert_eq!(request.start_payload.as_ref().unwrap().spd, Some(10));
        assert_eq!(request.start_payload.as_ref().unwrap().pit, Some(8));
        assert_eq!(request.start_payload.as_ref().unwrap().vol, Some(7));
        assert_eq!(request.start_payload.as_ref().unwrap().aue, Some(3));
    }

    #[test]
    fn test_request_with_default_payload() {
        let payload = SystemStartPayload::default();
        let request = BaiduTtsRequest::new("测试").with_payload(payload);

        let p = request.start_payload.unwrap();
        assert_eq!(p.spd, Some(6));
        assert_eq!(p.pit, Some(6));
        assert_eq!(p.vol, Some(5));
        assert_eq!(p.aue, Some(4)); // PCM-16k
    }

    #[test]
    fn test_request_clone() {
        let payload = SystemStartPayload {
            spd: Some(10),
            pit: Some(8),
            vol: Some(7),
            aue: Some(3),
            audio_ctrl: Some(r#"{"sampling_rate":24000}"#.to_string()),
        };

        let request = BaiduTtsRequest::new("测试文本").with_payload(payload).with_per("4148");
        let cloned = request.clone();

        assert_eq!(cloned.text, request.text);
        assert_eq!(
            cloned.start_payload.as_ref().unwrap().spd,
            request.start_payload.as_ref().unwrap().spd
        );
        assert_eq!(cloned.per, request.per);
    }

    #[test]
    fn test_request_with_per() {
        let request = BaiduTtsRequest::new("测试").with_per("4148");
        assert_eq!(request.per.as_deref(), Some("4148"));
    }

    // ============== 文本长度验证测试 ==============

    #[test]
    fn test_text_length_validation_exact_1000() {
        // 正好 1000 字应该是允许的
        let text: String = "测".repeat(1000);
        assert_eq!(text.chars().count(), 1000);
    }

    #[test]
    fn test_text_length_validation_over_1000() {
        // 超过 1000 字应该被拒绝
        let long_text: String = "测".repeat(1001);
        assert!(long_text.chars().count() > 1000);
    }

    #[test]
    fn test_text_length_validation_ascii() {
        // ASCII 字符也应该正确计数
        let text: String = "a".repeat(1000);
        assert_eq!(text.chars().count(), 1000);
    }

    #[test]
    fn test_text_length_validation_mixed() {
        // 混合字符应该正确计数
        let text = "Hello你好World世界".to_string();
        assert_eq!(text.chars().count(), 14);
    }

    // ============== BaiduTtsClient 测试 ==============

    #[test]
    fn test_client_creation() {
        let config = test_config();
        let client = BaiduTtsClient::new(config);
        assert_eq!(client.config().per.as_deref(), Some("4189"));
    }

    #[test]
    fn test_client_config_access() {
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: Some("1234".to_string()),
            spd: 10,
            pit: 8,
            vol: 12,
        };

        let client = BaiduTtsClient::new(config);
        let cfg = client.config();

        assert_eq!(cfg.per.as_deref(), Some("1234"));
        assert_eq!(cfg.spd, 10);
        assert_eq!(cfg.pit, 8);
        assert_eq!(cfg.vol, 12);
    }

    #[test]
    fn test_client_clone() {
        let config = test_config();
        let client = BaiduTtsClient::new(config);
        let cloned = client.clone();

        assert_eq!(cloned.config().per, client.config().per);
    }

    // ============== synthesize 输入验证测试 ==============

    #[test]
    fn test_synthesize_text_too_long_error() {
        let config = test_config();
        let client = BaiduTtsClient::new(config);
        let long_text: String = "测".repeat(1001);
        let request = BaiduTtsRequest::new(long_text);

        // synthesize 应该返回错误
        let result = client.synthesize(request);
        assert!(result.is_err());

        // 验证错误信息包含 "1000"
        match result {
            Err(err) => assert!(err.to_string().contains("1000")),
            Ok(_) => panic!("Expected error for text > 1000 chars"),
        }
    }

    #[test]
    fn test_synthesize_text_exactly_1000_chars() {
        let config = test_config();
        let client = BaiduTtsClient::new(config);
        let text: String = "测".repeat(1000);
        let request = BaiduTtsRequest::new(text);

        // 正好 1000 字应该不会在验证阶段报错
        // 注意：这会尝试建立网络连接，但我们只测试输入验证
        let result = client.synthesize(request);
        // 应该不会因为文本长度而失败（可能因为网络失败）
        assert!(result.is_ok());
    }

    // ============== 特殊字符测试 ==============

    #[test]
    fn test_request_with_special_characters() {
        let request = BaiduTtsRequest::new("Hello! 你好！@#$%^&*()");
        assert_eq!(request.text, "Hello! 你好！@#$%^&*()");
    }

    #[test]
    fn test_request_with_newlines() {
        let request = BaiduTtsRequest::new("第一行\n第二行\r\n第三行");
        assert!(request.text.contains('\n'));
    }

    #[test]
    fn test_request_with_unicode() {
        let request = BaiduTtsRequest::new("emoji: 😀🎉🚀 日本語: こんにちは");
        assert!(request.text.contains("😀"));
        assert!(request.text.contains("こんにちは"));
    }
}
