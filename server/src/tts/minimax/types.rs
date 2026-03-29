//! MiniMax TTS 类型定义

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 音频块
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// 音频数据
    pub data: Vec<u8>,
    /// 序列ID
    pub sequence_id: u64,
    /// 是否为最后一个块
    pub is_final: bool,
    /// 🆕 句子文本（用于在首帧前发送文字空块）
    /// 仅在"文字控制块"时携带，普通音频帧为 None
    pub sentence_text: Option<String>,
    /// 🆕 增益 dB 值（在引擎生成时确定，MiniMax=7.0, VolcEngine=0.0）
    pub gain_db: f32,
    /// 🆕 采样率（Hz）- MiniMax=44100, Baidu/Volc=16000
    /// 用于下游决定是否需要降采样
    pub sample_rate: u32,
}

/// 常见 TTS 采样率
pub const SAMPLE_RATE_44100: u32 = 44100; // MiniMax
pub const SAMPLE_RATE_16000: u32 = 16000; // Baidu, VolcEngine

impl AudioChunk {
    pub fn new(data: Vec<u8>, sequence_id: u64, is_final: bool) -> Self {
        // 默认使用 44100Hz（兼容 MiniMax）
        Self {
            data,
            sequence_id,
            is_final,
            sentence_text: None,
            gain_db: 0.0,
            sample_rate: SAMPLE_RATE_44100,
        }
    }

    /// 创建指定采样率的音频块
    pub fn new_with_sample_rate(data: Vec<u8>, sequence_id: u64, is_final: bool, sample_rate: u32) -> Self {
        Self { data, sequence_id, is_final, sentence_text: None, gain_db: 0.0, sample_rate }
    }

    /// 创建带增益的音频块
    pub fn new_with_gain(data: Vec<u8>, sequence_id: u64, is_final: bool, gain_db: f32) -> Self {
        Self {
            data,
            sequence_id,
            is_final,
            sentence_text: None,
            gain_db,
            sample_rate: SAMPLE_RATE_44100,
        }
    }

    /// 创建带增益和采样率的音频块
    pub fn new_with_gain_and_sample_rate(data: Vec<u8>, sequence_id: u64, is_final: bool, gain_db: f32, sample_rate: u32) -> Self {
        Self { data, sequence_id, is_final, sentence_text: None, gain_db, sample_rate }
    }

    /// 🆕 创建带文字的空块（用于在首帧前发送文字信令）
    pub fn new_text_marker(sentence_text: String, sequence_id: u64) -> Self {
        Self {
            data: Vec::new(),
            sequence_id,
            is_final: false,
            sentence_text: Some(sentence_text),
            gain_db: 0.0,
            sample_rate: SAMPLE_RATE_44100,
        }
    }

    /// 创建带增益和文字的空块
    pub fn new_text_marker_with_gain(sentence_text: String, sequence_id: u64, gain_db: f32) -> Self {
        Self {
            data: Vec::new(),
            sequence_id,
            is_final: false,
            sentence_text: Some(sentence_text),
            gain_db,
            sample_rate: SAMPLE_RATE_44100,
        }
    }
}

/// MiniMax错误类型
#[derive(Error, Debug)]
pub enum MiniMaxError {
    #[error("HTTP请求错误: {0}")]
    Http(#[from] reqwest::Error),

    #[error("WebSocket错误: {0}")]
    WebSocket(String),

    #[error("JSON解析错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("认证错误: {0}")]
    Auth(String),

    #[error("API错误: {0}")]
    Api(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("其他错误: {0}")]
    Other(String),
}

// ============== 客户端发送的消息 ==============

/// 任务开始请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStartRequest {
    /// 事件类型：task_start
    pub event: String,
    /// 请求的模型版本
    pub model: String,
    /// 音色设置
    pub voice_setting: VoiceSetting,
    /// 音频设置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_setting: Option<AudioSetting>,
    /// 发音字典
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pronunciation_dict: Option<PronunciationDict>,
    /// 音色权重（用于混合音色）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timbre_weights: Option<Vec<TimbreWeight>>,
    /// 语言增强
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_boost: Option<String>,
    /// 声音效果器设置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_modify: Option<serde_json::Value>,
}

/// 任务继续请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContinueRequest {
    /// 事件类型：task_continue
    pub event: String,
    /// 需要合成语音的文本，长度限制小于 10,000 字符
    pub text: String,
}

/// 任务结束请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFinishRequest {
    /// 事件类型：task_finish
    pub event: String,
}

// ============== 服务器响应的消息 ==============

/// WebSocket 响应消息（服务器 -> 客户端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum WebSocketResponse {
    /// 建连成功
    #[serde(rename = "connected_success")]
    ConnectedSuccess {
        /// 会话ID
        session_id: String,
        /// 追踪ID
        trace_id: String,
        /// 基础响应
        base_resp: BaseResponse,
    },
    /// 任务开始确认
    #[serde(rename = "task_started")]
    TaskStarted {
        /// 会话ID
        session_id: String,
        /// 追踪ID
        trace_id: String,
        /// 基础响应
        base_resp: BaseResponse,
    },
    /// 任务继续响应（包含音频数据）
    #[serde(rename = "task_continued")]
    TaskContinued {
        /// 会话ID
        session_id: String,
        /// 追踪ID
        trace_id: String,
        /// 音频数据（可能为 null）
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<TaskContinuedData>,
        /// 是否完结
        #[serde(skip_serializing_if = "Option::is_none")]
        is_final: Option<bool>,
        /// 额外信息
        #[serde(skip_serializing_if = "Option::is_none")]
        extra_info: Option<ExtraInfo>,
        /// 基础响应
        base_resp: BaseResponse,
    },
    /// 任务结束确认
    #[serde(rename = "task_finished")]
    TaskFinished {
        /// 会话ID
        session_id: String,
        /// 追踪ID
        trace_id: String,
        /// 基础响应
        base_resp: BaseResponse,
    },
    /// 任务失败
    #[serde(rename = "task_failed")]
    TaskFailed {
        /// 会话ID
        session_id: String,
        /// 追踪ID
        trace_id: String,
        /// 基础响应
        base_resp: BaseResponse,
    },
}

/// 任务继续响应中的数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskContinuedData {
    /// base64 编码的音频数据
    pub audio: String,
}

/// 额外信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtraInfo {
    /// 音频时长，精确到毫秒
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_length: Option<u64>,
    /// 音频采样率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_sample_rate: Option<u32>,
    /// 音频文件大小，单位为字节
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_size: Option<u64>,
    /// 音频比特率
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    /// 生成音频文件的格式（mp3/pcm/flac）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_format: Option<String>,
    /// 生成音频声道数（1：单声道，2：双声道）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channel: Option<u8>,
    /// 非法字符占比
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invisible_character_ratio: Option<f64>,
    /// 计费字符数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_characters: Option<u64>,
    /// 已发音的字数统计
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u64>,
}

// ============== 配置结构体 ==============

/// 自定义反序列化函数：将 WebSocket 传入的 [-1, 1] 浮点数转换为 MiniMax API 需要的 [-12, 12] 整数
/// 映射规则：pitch_float * 12.0 -> pitch_int，然后限制在 [-12, 12] 范围内
fn deserialize_pitch<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Visitor;
    use std::fmt;

    struct PitchVisitor;

    impl<'de> Visitor<'de> for PitchVisitor {
        type Value = Option<i32>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an integer or float that can be converted to i32")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            deserializer.deserialize_any(PitchValueVisitor).map(Some)
        }
    }

    struct PitchValueVisitor;

    impl<'de> Visitor<'de> for PitchValueVisitor {
        type Value = i32;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("an integer or float")
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            // 如果已经是整数，假设它已经在 [-12, 12] 范围内，直接转换
            let result: i32 = v
                .try_into()
                .map_err(|_| E::custom(format!("pitch value {} out of range for i32", v)))?;
            // 限制在 [-12, 12] 范围内
            Ok(result.clamp(-12, 12))
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            // 无符号整数，假设是 0-12 范围
            let result: i32 = v
                .try_into()
                .map_err(|_| E::custom(format!("pitch value {} out of range for i32", v)))?;
            Ok(result.clamp(0, 12))
        }

        fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            // 将 [-1, 1] 范围的浮点数映射到 [-12, 12] 整数
            // 公式：pitch_int = (pitch_float * 12.0).round()
            let mapped = (v * 12.0).round();
            let result = mapped as i64;
            let pitch_int: i32 = result
                .try_into()
                .map_err(|_| E::custom(format!("pitch value {} (mapped from {}) out of range for i32", result, v)))?;
            // 限制在 [-12, 12] 范围内
            Ok(pitch_int.clamp(-12, 12))
        }
    }

    deserializer.deserialize_option(PitchVisitor)
}

/// 音色设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSetting {
    /// 合成音频的音色编号
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// 合成音频的语速，取值范围 [0.5, 2]，默认值为 1.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    /// 合成音频的音量，取值范围 (0, 10]，默认值为 1.0
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vol: Option<f64>,
    /// 合成音频的语调，取值范围 [-12, 12]，默认值为 0
    /// 注意：WebSocket 传入的是 [-1, 1] 范围的浮点数，会被自动映射到 [-12, 12] 整数以符合 MiniMax API 要求
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "deserialize_pitch")]
    pub pitch: Option<i32>,
    /// 控制合成语音的情绪
    #[serde(skip_serializing_if = "Option::is_none")]
    pub emotion: Option<String>,
    /// 英语文本规范化，默认值为 false
    #[serde(skip_serializing_if = "Option::is_none")]
    pub english_normalization: Option<bool>,
    /// 控制是否朗读 latex 公式，默认为 false
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latex_read: Option<bool>,
}

impl Default for VoiceSetting {
    fn default() -> Self {
        Self {
            // 🔧 修复：提供默认 voice_id，避免 MiniMax API 返回 "invalid params, empty field" 错误
            voice_id: Some("wanwanxiaohe_moon".to_string()),
            speed: Some(1.0),
            vol: Some(1.0),
            pitch: Some(0),
            emotion: Some("fluent".to_string()),
            english_normalization: Some(false),
            latex_read: Some(false),
        }
    }
}

/// 音频设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSetting {
    /// 生成音频的采样率。可选范围 [8000, 16000, 22050, 24000, 32000, 44100]，默认为 32000
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// 生成音频的比特率。可选范围 [32000, 64000, 128000, 256000]，默认值为 128000。该参数仅对 mp3 格式的音频生效
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<u32>,
    /// 生成音频的格式
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// 生成音频的声道数。可选范围：[1, 2]，其中 1 为单声道，2 为双声道，默认值为 1
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
}

impl Default for AudioSetting {
    fn default() -> Self {
        Self {
            sample_rate: Some(44100),
            bitrate: None,
            format: Some("pcm".to_string()),
            channel: Some(1),
        }
    }
}

/// 发音字典
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PronunciationDict {
    /// 定义需要特殊标注的文字或符号对应的注音或发音替换规则
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tone: Option<Vec<String>>,
}

/// 音色权重（用于混合音色）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimbreWeight {
    /// 合成音频的音色编号
    pub voice_id: String,
    /// 合成音频各音色所占的权重，可选值范围为 [1, 100]
    pub weight: u8,
}

/// API基础响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseResponse {
    /// 状态码：0-正常，其他值表示错误
    pub status_code: i64,
    /// 状态消息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_msg: Option<String>,
}

impl BaseResponse {
    pub fn is_success(&self) -> bool {
        self.status_code == 0
    }

    /// 获取错误信息
    pub fn error_message(&self) -> String {
        match &self.status_msg {
            Some(msg) => format!("状态码 {}: {}", self.status_code, msg),
            None => format!("状态码: {}", self.status_code),
        }
    }
}

// ============== HTTP API 响应类型 ==============

/// 音色克隆响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCloneResponse {
    /// 输入音频是否命中风控
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_sensitive: Option<InputSensitive>,
    /// 试听音频链接（如果提供了text和model）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demo_audio: Option<String>,
    /// 基础响应
    pub base_resp: BaseResponse,
}

/// 输入音频风控信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputSensitive {
    /// 风控类型：0-正常，1-严重违规，2-色情，3-广告，4-违禁，5-谩骂，6-暴恐，7-其他
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<i32>,
}

/// 文件上传响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadResponse {
    /// 文件信息
    pub file: FileInfo,
}

/// 文件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    /// 文件ID
    pub file_id: i64,
}
