//! 百度 TTS REST API 客户端（HTTP 短文本合成）
//!
//! ## 接口说明
//!
//! - 端点: `https://tsn.baidu.com/text2audio`
//! - 方式: POST (推荐)
//! - 文本限制: 建议不超过 60 汉字，最长 1024 GBK 字节
//!
//! ## 优点
//!
//! - 无状态 HTTP 请求，可并发执行
//! - 无 WebSocket 连接管理开销
//! - 简单可靠

use crate::tts::minimax::AudioChunk;
use anyhow::{Context, Result, anyhow};
use async_stream::try_stream;
use futures_util::stream::Stream;
use reqwest::Client;
use serde::Deserialize;
use std::pin::Pin;
use std::time::Duration;
use tracing::{debug, info, warn};

use super::config::{BaiduTtsConfig, FIXED_SAMPLE_RATE_16K};
use super::types::BaiduTtsError;

/// REST API 端点
const REST_API_ENDPOINT: &str = "https://tsn.baidu.com/text2audio";

/// 请求超时时间（秒）
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// REST API 错误响应
#[derive(Debug, Deserialize)]
struct RestApiError {
    err_no: i64,
    err_msg: String,
    #[allow(dead_code)]
    sn: Option<String>,
    #[allow(dead_code)]
    idx: Option<i32>,
}

/// 百度 TTS HTTP 合成请求参数
#[derive(Debug, Clone)]
pub struct BaiduHttpTtsRequest {
    /// 待合成文本
    pub text: String,
    /// 可选：覆盖默认发音人 (per)
    pub per: Option<String>,
    /// 可选：覆盖语速 (0-15)
    pub spd: Option<u8>,
    /// 可选：覆盖音调 (0-15)
    pub pit: Option<u8>,
    /// 可选：覆盖音量 (0-15，精品音库可到15)
    pub vol: Option<u8>,
}

impl BaiduHttpTtsRequest {
    /// 创建新的合成请求
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), per: None, spd: None, pit: None, vol: None }
    }

    /// 设置发音人
    pub fn with_per(mut self, per: impl Into<String>) -> Self {
        self.per = Some(per.into());
        self
    }

    /// 设置语速
    pub fn with_spd(mut self, spd: u8) -> Self {
        self.spd = Some(spd.min(15));
        self
    }

    /// 设置音调
    pub fn with_pit(mut self, pit: u8) -> Self {
        self.pit = Some(pit.min(15));
        self
    }

    /// 设置音量
    pub fn with_vol(mut self, vol: u8) -> Self {
        self.vol = Some(vol.min(15));
        self
    }

    /// 从旧的 WebSocket 请求格式转换
    pub fn from_ws_request(ws_req: &super::client::BaiduTtsRequest, config: &BaiduTtsConfig) -> Self {
        let mut req = Self::new(ws_req.text.clone());

        // per 优先级: 请求指定 > 配置默认
        if let Some(ref per) = ws_req.per {
            req.per = Some(per.clone());
        } else if let Some(ref per) = config.per {
            req.per = Some(per.clone());
        }

        // prosody 参数：从 payload 或 config 获取
        if let Some(ref payload) = ws_req.start_payload {
            req.spd = payload.spd;
            req.pit = payload.pit;
            req.vol = payload.vol;
        } else {
            req.spd = Some(config.spd);
            req.pit = Some(config.pit);
            req.vol = Some(config.vol);
        }

        req
    }
}

/// 百度 TTS HTTP 客户端
#[derive(Clone)]
pub struct BaiduHttpTtsClient {
    config: BaiduTtsConfig,
    http_client: Client,
}

impl BaiduHttpTtsClient {
    /// 创建新的客户端实例
    pub fn new(config: BaiduTtsConfig) -> Self {
        let http_client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .pool_max_idle_per_host(4) // 连接池复用
            .build()
            .expect("Failed to create HTTP client");

        Self { config, http_client }
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

    /// 执行语音合成，返回音频块流
    ///
    /// REST API 返回完整音频，但为兼容流式接口，
    /// 将结果包装为单个 AudioChunk 的流
    pub fn synthesize(&self, request: BaiduHttpTtsRequest) -> Result<Pin<Box<dyn Stream<Item = Result<AudioChunk>> + Send + '_>>> {
        let config = self.config.clone();
        let http_client = self.http_client.clone();
        let text = request.text.clone();

        // 验证文本长度（REST API 限制 1024 GBK 字节）
        let gbk_len = text.len(); // 近似，实际应转 GBK
        if gbk_len > 1024 {
            return Err(anyhow!(BaiduTtsError::TextTooLong(format!(
                "文本过长 ({} bytes)，REST API 限制 1024 GBK 字节",
                gbk_len
            ))));
        }

        // 验证 per 参数
        let per = request
            .per
            .or_else(|| config.per.clone())
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(BaiduTtsError::Config(
                    "缺少 per：请设置 BAIDU_TTS_PER 或在请求里指定 per".to_string()
                ))
            })?;

        let stream = try_stream! {
            // 获取 access_token
            let access_token = config.get_access_token().await
                .context("获取百度 TTS Access Token 失败")?;

            // 构建请求参数
            // tex 需要两次 URL 编码
            let tex_once = urlencoding::encode(&text);
            let tex_twice = urlencoding::encode(&tex_once);

            let spd = request.spd.unwrap_or(config.spd);
            let pit = request.pit.unwrap_or(config.pit);
            let vol = request.vol.unwrap_or(config.vol);

            // 生成唯一 cuid
            let cuid = format!("be1-tts-{}", nanoid::nanoid!(8));

            let form_body = format!(
                "tex={}&tok={}&cuid={}&ctp=1&lan=zh&spd={}&pit={}&vol={}&per={}&aue=4&audio_ctrl={}",
                tex_twice,
                access_token,
                cuid,
                spd,
                pit,
                vol,
                per,
                urlencoding::encode(r#"{"sampling_rate":16000}"#)
            );

            debug!(
                "📤 百度 TTS HTTP: 发送请求, text='{}', per={}, spd={}, pit={}, vol={}",
                text.chars().take(30).collect::<String>(),
                per,
                spd,
                pit,
                vol
            );

            let start_time = std::time::Instant::now();

            let response = http_client
                .post(REST_API_ENDPOINT)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(form_body)
                .send()
                .await
                .context("百度 TTS HTTP 请求失败")?;

            let status = response.status();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();

            debug!("📥 百度 TTS HTTP: status={}, content_type={}", status, content_type);

            // 检查是否为音频响应
            if content_type.starts_with("audio") {
                let audio_data = response.bytes().await
                    .context("读取音频数据失败")?;

                let elapsed = start_time.elapsed();
                info!(
                    "✅ 百度 TTS HTTP: 合成完成, {} bytes, {}ms, text='{}'",
                    audio_data.len(),
                    elapsed.as_millis(),
                    text.chars().take(20).collect::<String>()
                );

                // 返回音频数据
                let chunk = AudioChunk {
                    data: audio_data.to_vec(),
                    sequence_id: 0,
                    is_final: false,
                    sentence_text: Some(text.clone()),
                    gain_db: 0.0,
                    sample_rate: FIXED_SAMPLE_RATE_16K,
                };
                yield chunk;

                // 发送 final 标记
                let final_chunk = AudioChunk {
                    data: Vec::new(),
                    sequence_id: u64::MAX,
                    is_final: true,
                    sentence_text: None,
                    gain_db: 0.0,
                    sample_rate: FIXED_SAMPLE_RATE_16K,
                };
                yield final_chunk;
            } else {
                // 错误响应
                let body = response.text().await
                    .context("读取错误响应失败")?;

                if let Ok(err) = serde_json::from_str::<RestApiError>(&body) {
                    warn!(
                        "❌ 百度 TTS HTTP 错误: err_no={}, err_msg='{}', sn={:?}",
                        err.err_no, err.err_msg, err.sn
                    );
                    Err(anyhow!(BaiduTtsError::Server {
                        code: err.err_no,
                        message: err.err_msg,
                    }))?;
                } else {
                    warn!("❌ 百度 TTS HTTP 未知错误: status={}, body={}", status, body);
                    Err(anyhow!(BaiduTtsError::Other(format!(
                        "HTTP {}: {}",
                        status, body
                    ))))?;
                }
            }
        };

        Ok(Box::pin(stream))
    }

    /// 预热连接（发送一个简单请求预热 HTTP 连接池）
    pub async fn prewarm_connection(&self) -> Result<()> {
        // 对于 HTTP 客户端，预热主要是建立 TCP 连接
        // 可以发送一个 HEAD 请求或简单 GET
        let _ = self.config.get_access_token().await?;
        info!("🔥 百度 TTS HTTP: 连接预热完成（Token 已缓存）");
        Ok(())
    }

    /// 并发预热（HTTP 客户端只需预热一次 token）
    pub async fn prewarm_connections(&self, _count: usize) -> Result<()> {
        self.prewarm_connection().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> BaiduTtsConfig {
        BaiduTtsConfig {
            api_key: "test_key".to_string(),
            secret_key: "test_secret".to_string(),
            per: Some("4148".to_string()),
            spd: 5,
            pit: 5,
            vol: 5,
        }
    }

    #[test]
    fn test_request_creation() {
        let req = BaiduHttpTtsRequest::new("测试文本");
        assert_eq!(req.text, "测试文本");
        assert!(req.per.is_none());
    }

    #[test]
    fn test_request_with_params() {
        let req = BaiduHttpTtsRequest::new("测试")
            .with_per("4148")
            .with_spd(7)
            .with_pit(4)
            .with_vol(8);

        assert_eq!(req.per.as_deref(), Some("4148"));
        assert_eq!(req.spd, Some(7));
        assert_eq!(req.pit, Some(4));
        assert_eq!(req.vol, Some(8));
    }

    #[test]
    fn test_request_param_clamping() {
        let req = BaiduHttpTtsRequest::new("测试")
            .with_spd(20) // 应该被限制到 15
            .with_pit(255);

        assert_eq!(req.spd, Some(15));
        assert_eq!(req.pit, Some(15));
    }

    #[test]
    fn test_client_creation() {
        let config = test_config();
        let client = BaiduHttpTtsClient::new(config);
        assert_eq!(client.config().per.as_deref(), Some("4148"));
    }

    #[test]
    fn test_text_too_long_validation() {
        let config = test_config();
        let client = BaiduHttpTtsClient::new(config);

        // 创建超长文本
        let long_text: String = "测".repeat(600); // 约 1800 bytes
        let req = BaiduHttpTtsRequest::new(long_text);

        let result = client.synthesize(req);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_per_validation() {
        let config = BaiduTtsConfig {
            api_key: "k".to_string(),
            secret_key: "s".to_string(),
            per: None, // 无默认 per
            spd: 5,
            pit: 5,
            vol: 5,
        };
        let client = BaiduHttpTtsClient::new(config);

        let req = BaiduHttpTtsRequest::new("测试"); // 也没指定 per
        let result = client.synthesize(req);
        assert!(result.is_err());
    }
}
