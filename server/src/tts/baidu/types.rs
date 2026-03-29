//! 百度 TTS WebSocket 协议类型定义

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 百度 TTS 错误类型
#[derive(Error, Debug)]
pub enum BaiduTtsError {
    #[error("WebSocket 连接错误: {0}")]
    WebSocket(String),

    #[error("JSON 解析错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("认证错误: {0}")]
    Auth(String),

    #[error("参数错误: code={code}, message={message}")]
    Parameter { code: i64, message: String },

    #[error("文本过长: {0}")]
    TextTooLong(String),

    #[error("服务器错误: code={code}, message={message}")]
    Server { code: i64, message: String },

    #[error("配置错误: {0}")]
    Config(String),

    #[error("超时: {0}")]
    Timeout(String),

    #[error("其他错误: {0}")]
    Other(String),
}

/// 百度 TTS 错误码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaiduTtsErrorCode {
    /// 成功
    Success = 0,
    /// 参数缺失
    ParameterMissing = 216101,
    /// 参数错误（语速/音调/音量/音频格式）
    ParameterError = 216100,
    /// 文本过长
    TextTooLong = 216103,
    /// 待处理文本过长
    TextPendingTooLong = 216419,
    /// 鉴权失败
    AuthFailed = 401,
    /// 无访问权限
    Forbidden = 403,
    /// URL 错误
    NotFound = 404,
    /// 触发限流
    TooManyRequests = 429,
    /// 服务器内部错误
    InternalServerError = 500,
    /// 后端服务连接失败
    BadGateway = 502,
}

impl BaiduTtsErrorCode {
    /// 根据错误码获取描述信息
    pub fn message(code: i64) -> &'static str {
        match code {
            0 => "成功",
            216100 => "参数错误",
            216101 => "参数缺失",
            216103 => "文本过长，请控制在1000字以内",
            216419 => "当前待处理文本过长，请稍后发送",
            401 => "鉴权失败",
            403 => "无访问权限，接口功能未开通",
            404 => "输入的 URL 错误",
            429 => "触发限流",
            500 => "服务器内部错误",
            502 => "后端服务连接失败",
            _ => "未知错误",
        }
    }
}

// ============== 客户端发送的消息 ==============

/// 开始合成请求 (system.start)
#[derive(Debug, Clone, Serialize)]
pub struct SystemStartRequest {
    /// 固定值 "system.start"
    #[serde(rename = "type")]
    pub msg_type: String,
    /// 合成参数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<SystemStartPayload>,
}

impl Default for SystemStartRequest {
    fn default() -> Self {
        Self {
            msg_type: "system.start".to_string(),
            payload: Some(SystemStartPayload::default()),
        }
    }
}

/// 开始合成请求的 payload
#[derive(Debug, Clone, Serialize)]
pub struct SystemStartPayload {
    /// 语速，取值 0-15，默认为 6
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spd: Option<u8>,
    /// 音调，取值 0-15，默认为 6
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pit: Option<u8>,
    /// 音量，基础音库取值 0-9，其他音库取值 0-15，默认为 5
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vol: Option<u8>,
    /// 音频格式：3=mp3-16k/24k, 4=pcm-16k/24k, 5=pcm-8k, 6=wav-16k/24k，默认为 3
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aue: Option<u8>,
    /// 采样率控制，格式：{"sampling_rate":16000}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_ctrl: Option<String>,
}

impl Default for SystemStartPayload {
    fn default() -> Self {
        Self {
            spd: Some(6),
            pit: Some(6),
            vol: Some(5),
            aue: Some(4), // PCM-16k for consistency with other providers
            audio_ctrl: Some(r#"{"sampling_rate":16000}"#.to_string()),
        }
    }
}

/// 文本合成请求 (text)
#[derive(Debug, Clone, Serialize)]
pub struct TextRequest {
    /// 固定值 "text"
    #[serde(rename = "type")]
    pub msg_type: String,
    /// 文本内容
    pub payload: TextPayload,
}

impl TextRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self { msg_type: "text".to_string(), payload: TextPayload { text: text.into() } }
    }
}

/// 文本请求的 payload
#[derive(Debug, Clone, Serialize)]
pub struct TextPayload {
    /// 需要进行语音合成的文字，不超过 1000 字
    pub text: String,
}

/// 结束合成请求 (system.finish)
#[derive(Debug, Clone, Serialize)]
pub struct SystemFinishRequest {
    /// 固定值 "system.finish"
    #[serde(rename = "type")]
    pub msg_type: String,
}

impl Default for SystemFinishRequest {
    fn default() -> Self {
        Self { msg_type: "system.finish".to_string() }
    }
}

// ============== 服务器响应的消息 ==============

/// 服务器响应消息（通用）
#[derive(Debug, Clone, Deserialize)]
pub struct BaiduTtsResponse {
    /// 消息类型：system.started, system.finished, system.error
    #[serde(rename = "type")]
    pub msg_type: String,
    /// 错误码，0 表示成功
    #[serde(default)]
    pub code: i64,
    /// 错误信息
    #[serde(default)]
    pub message: String,
    /// 响应头信息
    #[serde(default)]
    pub headers: Option<BaiduTtsHeaders>,
}

impl BaiduTtsResponse {
    /// 检查响应是否成功
    pub fn is_success(&self) -> bool {
        self.code == 0
    }

    /// 获取错误信息
    pub fn error_message(&self) -> String {
        if self.message.is_empty() {
            BaiduTtsErrorCode::message(self.code).to_string()
        } else {
            self.message.clone()
        }
    }

    /// 获取 session_id
    pub fn session_id(&self) -> Option<&str> {
        self.headers.as_ref().and_then(|h| h.session_id.as_deref())
    }
}

/// 响应头信息
#[derive(Debug, Clone, Deserialize)]
pub struct BaiduTtsHeaders {
    /// 会话 ID
    #[serde(default)]
    pub session_id: Option<String>,
}

/// 百度 TTS 音频格式
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BaiduAudioFormat {
    /// MP3 格式，16k/24k 采样率
    Mp3 = 3,
    /// PCM 格式，16k/24k 采样率
    #[default]
    Pcm16k = 4,
    /// PCM 格式，8k 采样率
    Pcm8k = 5,
    /// WAV 格式，16k/24k 采样率
    Wav = 6,
}

impl From<BaiduAudioFormat> for u8 {
    fn from(format: BaiduAudioFormat) -> Self {
        format as u8
    }
}

impl TryFrom<u8> for BaiduAudioFormat {
    type Error = BaiduTtsError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            3 => Ok(Self::Mp3),
            4 => Ok(Self::Pcm16k),
            5 => Ok(Self::Pcm8k),
            6 => Ok(Self::Wav),
            _ => Err(BaiduTtsError::Parameter {
                code: 216100,
                message: format!("无效的音频格式: {}, 支持 3=mp3, 4=pcm-16k, 5=pcm-8k, 6=wav", value),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============== 错误码测试 ==============

    #[test]
    fn test_error_code_message() {
        assert_eq!(BaiduTtsErrorCode::message(0), "成功");
        assert_eq!(BaiduTtsErrorCode::message(216100), "参数错误");
        assert_eq!(BaiduTtsErrorCode::message(216101), "参数缺失");
        assert_eq!(BaiduTtsErrorCode::message(216103), "文本过长，请控制在1000字以内");
        assert_eq!(BaiduTtsErrorCode::message(216419), "当前待处理文本过长，请稍后发送");
        assert_eq!(BaiduTtsErrorCode::message(401), "鉴权失败");
        assert_eq!(BaiduTtsErrorCode::message(403), "无访问权限，接口功能未开通");
        assert_eq!(BaiduTtsErrorCode::message(404), "输入的 URL 错误");
        assert_eq!(BaiduTtsErrorCode::message(429), "触发限流");
        assert_eq!(BaiduTtsErrorCode::message(500), "服务器内部错误");
        assert_eq!(BaiduTtsErrorCode::message(502), "后端服务连接失败");
        assert_eq!(BaiduTtsErrorCode::message(99999), "未知错误");
    }

    // ============== 音频格式测试 ==============

    #[test]
    fn test_audio_format_default() {
        let format = BaiduAudioFormat::default();
        assert_eq!(format, BaiduAudioFormat::Pcm16k);
    }

    #[test]
    fn test_audio_format_to_u8() {
        assert_eq!(u8::from(BaiduAudioFormat::Mp3), 3);
        assert_eq!(u8::from(BaiduAudioFormat::Pcm16k), 4);
        assert_eq!(u8::from(BaiduAudioFormat::Pcm8k), 5);
        assert_eq!(u8::from(BaiduAudioFormat::Wav), 6);
    }

    #[test]
    fn test_audio_format_from_u8_valid() {
        assert_eq!(BaiduAudioFormat::try_from(3).unwrap(), BaiduAudioFormat::Mp3);
        assert_eq!(BaiduAudioFormat::try_from(4).unwrap(), BaiduAudioFormat::Pcm16k);
        assert_eq!(BaiduAudioFormat::try_from(5).unwrap(), BaiduAudioFormat::Pcm8k);
        assert_eq!(BaiduAudioFormat::try_from(6).unwrap(), BaiduAudioFormat::Wav);
    }

    #[test]
    fn test_audio_format_from_u8_invalid() {
        assert!(BaiduAudioFormat::try_from(0).is_err());
        assert!(BaiduAudioFormat::try_from(1).is_err());
        assert!(BaiduAudioFormat::try_from(2).is_err());
        assert!(BaiduAudioFormat::try_from(7).is_err());
        assert!(BaiduAudioFormat::try_from(255).is_err());
    }

    // ============== 请求序列化测试 ==============

    #[test]
    fn test_system_start_request_default() {
        let request = SystemStartRequest::default();
        assert_eq!(request.msg_type, "system.start");
        assert!(request.payload.is_some());

        let payload = request.payload.unwrap();
        assert_eq!(payload.spd, Some(6));
        assert_eq!(payload.pit, Some(6));
        assert_eq!(payload.vol, Some(5));
        assert_eq!(payload.aue, Some(4));
    }

    #[test]
    fn test_system_start_request_serialization() {
        let request = SystemStartRequest::default();
        let json = serde_json::to_string(&request).unwrap();

        assert!(json.contains(r#""type":"system.start""#));
        assert!(json.contains(r#""spd":6"#));
        assert!(json.contains(r#""pit":6"#));
        assert!(json.contains(r#""vol":5"#));
        assert!(json.contains(r#""aue":4"#));
    }

    #[test]
    fn test_system_start_payload_custom() {
        let payload = SystemStartPayload {
            spd: Some(10),
            pit: Some(8),
            vol: Some(15),
            aue: Some(3),
            audio_ctrl: Some(r#"{"sampling_rate":24000}"#.to_string()),
        };

        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains(r#""spd":10"#));
        assert!(json.contains(r#""pit":8"#));
        assert!(json.contains(r#""vol":15"#));
        assert!(json.contains(r#""aue":3"#));
        assert!(json.contains("24000"));
    }

    #[test]
    fn test_text_request_new() {
        let request = TextRequest::new("测试文本");
        assert_eq!(request.msg_type, "text");
        assert_eq!(request.payload.text, "测试文本");
    }

    #[test]
    fn test_text_request_serialization() {
        let request = TextRequest::new("你好世界");
        let json = serde_json::to_string(&request).unwrap();

        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""text":"你好世界""#));
    }

    #[test]
    fn test_system_finish_request_default() {
        let request = SystemFinishRequest::default();
        assert_eq!(request.msg_type, "system.finish");
    }

    #[test]
    fn test_system_finish_request_serialization() {
        let request = SystemFinishRequest::default();
        let json = serde_json::to_string(&request).unwrap();

        assert!(json.contains(r#""type":"system.finish""#));
    }

    // ============== 响应反序列化测试 ==============

    #[test]
    fn test_response_success_deserialization() {
        let json = r#"{
            "type": "system.started",
            "code": 0,
            "message": "success",
            "headers": {
                "session_id": "test_session_123"
            }
        }"#;

        let response: BaiduTtsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.msg_type, "system.started");
        assert_eq!(response.code, 0);
        assert!(response.is_success());
        assert_eq!(response.session_id(), Some("test_session_123"));
    }

    #[test]
    fn test_response_error_deserialization() {
        let json = r#"{
            "type": "system.error",
            "code": 216103,
            "message": "文本过长, 请控制在1000字以内",
            "headers": {
                "session_id": "test_session_456"
            }
        }"#;

        let response: BaiduTtsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.msg_type, "system.error");
        assert_eq!(response.code, 216103);
        assert!(!response.is_success());
        assert_eq!(response.error_message(), "文本过长, 请控制在1000字以内");
    }

    #[test]
    fn test_response_error_message_fallback() {
        let json = r#"{
            "type": "system.error",
            "code": 216100,
            "message": ""
        }"#;

        let response: BaiduTtsResponse = serde_json::from_str(json).unwrap();
        // 当 message 为空时，应该返回错误码对应的默认消息
        assert_eq!(response.error_message(), "参数错误");
    }

    #[test]
    fn test_response_minimal_deserialization() {
        // 测试最小化的响应（只有必填字段）
        let json = r#"{"type": "system.finished"}"#;

        let response: BaiduTtsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.msg_type, "system.finished");
        assert_eq!(response.code, 0); // 默认值
        assert!(response.message.is_empty()); // 默认值
        assert!(response.headers.is_none());
        assert!(response.session_id().is_none());
    }

    #[test]
    fn test_response_without_session_id() {
        let json = r#"{
            "type": "system.started",
            "code": 0,
            "message": "success",
            "headers": {}
        }"#;

        let response: BaiduTtsResponse = serde_json::from_str(json).unwrap();
        assert!(response.session_id().is_none());
    }

    // ============== 错误类型测试 ==============

    #[test]
    fn test_error_display() {
        let error = BaiduTtsError::WebSocket("连接失败".to_string());
        assert!(error.to_string().contains("WebSocket"));
        assert!(error.to_string().contains("连接失败"));

        let error = BaiduTtsError::TextTooLong("超过1000字".to_string());
        assert!(error.to_string().contains("文本过长"));

        let error = BaiduTtsError::Parameter { code: 216100, message: "语速参数错误".to_string() };
        assert!(error.to_string().contains("216100"));
        assert!(error.to_string().contains("语速参数错误"));

        let error = BaiduTtsError::Server { code: 500, message: "内部错误".to_string() };
        assert!(error.to_string().contains("500"));
    }
}
