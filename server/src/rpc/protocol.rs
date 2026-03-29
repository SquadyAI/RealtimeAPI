//! 定义RPC系统的核心协议：支持WebSocket JSON格式和二进制格式
use nanoid::nanoid;
use serde::{Deserialize, Serialize};
use thiserror::Error;
/// 协议常量
// NANOID_PROTOCOLID_COMMANDID_RESERVED, 总header长度是32位
pub const NANOID_SIZE: usize = 16;
pub const PROTOCOL_ID_OFFSET: usize = NANOID_SIZE;
pub const COMMAND_ID_OFFSET: usize = NANOID_SIZE + 1;
pub const BINARY_HEADER_SIZE: usize = 32;

/// 协议ID，用于区分不同的业务类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
#[repr(u8)]
pub enum ProtocolId {
    Asr = 1,
    Llm = 2,
    Tts = 3,
    Translation = 4,
    All = 100,
}

impl From<u8> for ProtocolId {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Asr,
            2 => Self::Llm,
            3 => Self::Tts,
            4 => Self::Translation,
            100 => Self::All,
            _ => {
                tracing::warn!("未知协议ID: {}, 默认为All", value);
                Self::All
            },
        }
    }
}

impl From<ProtocolId> for u8 {
    fn from(id: ProtocolId) -> Self {
        id as u8
    }
}

impl ProtocolId {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(ProtocolId::Asr),
            2 => Some(ProtocolId::Llm),
            3 => Some(ProtocolId::Tts),
            4 => Some(ProtocolId::Translation),
            100 => Some(ProtocolId::All),
            _ => None,
        }
    }
}

/// 命令ID定义
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "u8", into = "u8")]
#[repr(u8)]
pub enum CommandId {
    Start = 1,
    Stop = 2,
    AudioChunk = 3,
    TextData = 4,
    StopInput = 5, // 🆕 停止文本输入（但不停止会话）
    ImageData = 6, // 🆕 视觉输入（单帧图像二进制）
    /// 🆕 用户按钮打断：停止当前 ASR/LLM/TTS 输出，但不销毁会话
    Interrupt = 7,
    ResponseAudioDelta = 20, // 🆕 响应音频增量（二进制格式）
    Result = 100,
    Error = 255,
}

impl From<u8> for CommandId {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Start,
            2 => Self::Stop,
            3 => Self::AudioChunk,
            4 => Self::TextData,
            5 => Self::StopInput,
            6 => Self::ImageData,
            7 => Self::Interrupt,
            20 => Self::ResponseAudioDelta,
            100 => Self::Result,
            255 => Self::Error,
            _ => {
                tracing::warn!("未知命令ID: {}, 默认为Error", value);
                Self::Error
            },
        }
    }
}

impl From<CommandId> for u8 {
    fn from(id: CommandId) -> Self {
        id as u8
    }
}

impl CommandId {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(CommandId::Start),
            2 => Some(CommandId::Stop),
            3 => Some(CommandId::AudioChunk),
            4 => Some(CommandId::TextData),
            5 => Some(CommandId::StopInput),
            6 => Some(CommandId::ImageData),
            7 => Some(CommandId::Interrupt),
            20 => Some(CommandId::ResponseAudioDelta),
            100 => Some(CommandId::Result),
            255 => Some(CommandId::Error),
            _ => None,
        }
    }
}

/// 二进制协议头部
#[derive(Debug, Clone, PartialEq)]
pub struct BinaryHeader {
    /// 16字节的nanoid session ID
    pub session_id: String,
    /// 1字节的业务ID
    pub protocol_id: ProtocolId,
    /// 1字节的命令ID
    pub command_id: CommandId,
    /// 14字节的预留字段 (保持32字节头，便于将来扩展)
    pub reserved: [u8; 14],
}

/// 二进制消息格式
#[derive(Debug, Clone)]
pub struct BinaryMessage {
    pub header: BinaryHeader,
    pub payload: Vec<u8>,
}

/// WebSocket消息格式（JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSocketMessage {
    pub protocol_id: ProtocolId,
    pub command_id: CommandId,
    pub session_id: String,
    #[serde(default, deserialize_with = "deserialize_optional_payload")]
    pub payload: Option<MessagePayload>,
    /// 顶层可选：有些客户端会将 timezone 放在顶层
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// 顶层可选：有些客户端会将 location 放在顶层
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// 自定义反序列化函数：处理 payload 为空对象 {} 的情况
fn deserialize_optional_payload<'de, D>(deserializer: D) -> Result<Option<MessagePayload>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;

    match value {
        Some(serde_json::Value::Object(map)) => {
            if map.is_empty() {
                Ok(None)
            } else {
                // 重新序列化并解析为 MessagePayload
                // 这种方式虽然有性能开销，但能复用 MessagePayload 的反序列化逻辑
                // 考虑到这是边缘情况或控制消息，性能影响可控
                let v = serde_json::Value::Object(map);
                serde_json::from_value(v).map(Some).map_err(serde::de::Error::custom)
            }
        },
        Some(v) => serde_json::from_value(v).map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

/// 消息载荷类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum MessagePayload {
    #[serde(rename = "audio_chunk")]
    AudioChunk {
        // Base64编码的音频数据
        data: String,
        sample_rate: u32,
        channels: u16,
    },

    #[serde(rename = "text_data")]
    TextData {
        /// 文本内容 - 用于TTS直接输入
        text: String,
    },

    #[serde(rename = "session_config")]
    SessionConfig {
        /// 语音分段模式: "vad" | "ptt" (push-to-talk)
        #[serde(default)]
        mode: Option<String>,
        /// 可选：VAD检测阈值 (0.0 - 1.0)
        #[serde(default)]
        vad_threshold: Option<f32>,
        /// 在认定语音结束前的最小静音时长 (毫秒)
        #[serde(default)]
        silence_duration_ms: Option<u32>,
        /// 认定语音开始所需的最小连续语音时长 (毫秒)
        #[serde(default)]
        min_speech_duration_ms: Option<u32>,
        /// 系统提示词 - 用于LLM会话初始化
        #[serde(default)]
        system_prompt: Option<String>,
        /// MCP服务器配置 - 替代直接传入tools，从MCP服务器动态获取工具
        #[serde(default)]
        mcp_server_config: Option<serde_json::Value>,
        /// 工具端点URL - 从HTTP端点获取工具配置并缓存，转换为原有tools格式
        #[serde(default)]
        tools_endpoint: Option<String>,
        /// 🆕 三合一远程配置端点URL - 从该URL一次性获取 system_prompt, tools, mcp_server_config, search_config
        /// 优先级最高：prompt_endpoint > 其他字段
        #[serde(default)]
        prompt_endpoint: Option<String>,
        /// 传统的Function Call工具定义 - 与mcp_server_config和tools_endpoint互斥，优先使用MCP和tools_endpoint
        #[serde(default)]
        tools: Option<Vec<serde_json::Value>>,
        /// 离线工具列表 - 用于拒绝逻辑判断，当 tools 和 offline_tools 中都没有对应工具时才拒绝
        /// 支持的离线工具: take_photo, take_video, take_record, music_control, increase_volume, reduce_volume
        /// 兼容蛇形命名 offline_tools 和驼峰命名 offLineTools
        #[serde(default, alias = "offLineTools")]
        offline_tools: Option<Vec<String>>,
        /// 工具选择策略 - "auto" | "none" | {"type": "function", "function": {"name": "tool_name"}}
        #[serde(default)]
        tool_choice: Option<serde_json::Value>,
        /// 启用内置搜索引擎工具
        #[serde(default)]
        enable_search: Option<bool>,
        /// 搜索引擎配置
        #[serde(default)]
        search_config: Option<serde_json::Value>,
        /// TTS语音设置
        #[serde(default)]
        voice_setting: Option<serde_json::Value>,
        /// ASR语言偏好设置: "zh" | "en" | "yue" | "ja" | "ko" | "auto"
        #[serde(default)]
        asr_language: Option<String>,
        /// 用户时区设置: 如 "Asia/Shanghai", "America/New_York" 等 IANA 时区名称
        #[serde(default)]
        timezone: Option<String>,
        /// 用户位置信息: 如 "中国", "美国" 等国家或地区名称
        #[serde(default)]
        location: Option<String>,
        /// TTS音频分片大小（KB），默认4KB - 已弃用，请使用audio_slice_ms
        #[serde(default)]
        audio_chunk_size_kb: Option<u32>,
        /// 初始快速发送的块数
        #[serde(default)]
        initial_burst_count: Option<u32>,
        /// 初始快速发送时的延迟(ms)
        #[serde(default)]
        initial_burst_delay_ms: Option<u32>,
        /// 发送速率倍数 (例如 1.5 表示比实时快50%)
        #[serde(default)]
        send_rate_multiplier: Option<f64>,
        /// 🆕 完整的音频输出配置对象（推荐）
        #[serde(default)]
        output_audio_config: Option<serde_json::Value>,
        /// 🆕 音频输入处理器配置（JSON 对象，映射到 AudioInputConfig）
        #[serde(default)]
        input_audio_config: Option<serde_json::Value>,
        /// 🆕 当为 true 时，服务器在发送 response.text.done 时仅发送信令，不携带完整文本
        #[serde(default)]
        text_done_signal_only: Option<bool>,
        /// 🆕 当为 true 时，除了语音和工具调用之外的所有事件都不发送
        #[serde(default)]
        signal_only: Option<bool>,
        /// 🆕 ASR 繁简转换模式: "none" | "t2s" (繁→简) | "s2t" (简→繁)
        /// 对 ASR 输出生效，转换后的文本用于客户端事件、对话历史和 LLM 输入
        #[serde(default)]
        asr_chinese_convert: Option<String>,
        /// 🆕 TTS 繁简转换模式: "none" | "t2s" (繁→简) | "s2t" (简→繁)
        /// 仅对 TTS 输入生效，不影响存储的对话历史
        #[serde(default)]
        tts_chinese_convert: Option<String>,
        // 🔧 移除 defer_llm_until_stop_input 字段，现在通过 SpeechMode::VadDeferred 来处理
        /// 🆕 同声传译源语言: 如 "en", "zh", "ja" 等
        #[serde(default)]
        from_language: Option<String>,
        /// 🆕 同声传译目标语言: 如 "zh", "en", "ja" 等
        #[serde(default)]
        to_language: Option<String>,
    },

    // ===== 客户端回传消息类型 =====
    #[serde(rename = "conversation.item.create")]
    ConversationItemCreate {
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_item_id: Option<String>,
        item: serde_json::Value,
    },

    // ===== Realtime API Events =====
    #[serde(rename = "session.created")]
    SessionCreate { event_id: String, session: serde_json::Value },

    #[serde(rename = "conversation.item.created")]
    ConversationItemCreated {
        event_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        previous_item_id: Option<String>,
        item: serde_json::Value,
    },

    #[serde(rename = "conversation.item.updated")]
    ConversationItemUpdated { event_id: String, item: serde_json::Value },

    #[serde(rename = "input_audio_buffer.speech_started")]
    InputAudioSpeechStarted { event_id: String, audio_start_ms: u32, item_id: String },

    #[serde(rename = "input_audio_buffer.speech_stopped")]
    InputAudioSpeechStopped { event_id: String, audio_end_ms: u32, item_id: String },

    #[serde(rename = "conversation.item.input_audio_transcription.delta")]
    AsrTranscriptionDelta { event_id: String, item_id: String, content_index: u32, delta: String },

    #[serde(rename = "conversation.item.input_audio_transcription.completed")]
    AsrTranscriptionCompleted {
        event_id: String,
        item_id: String,
        content_index: u32,
        transcript: String,
    },

    #[serde(rename = "response.created")]
    ResponseCreated { event_id: String, response: serde_json::Value },

    #[serde(rename = "response.text.delta")]
    ResponseTextDelta {
        event_id: String,
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
        delta: String,
    },

    #[serde(rename = "response.text.done")]
    ResponseTextDone {
        event_id: String,
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
        text: String,
    },

    #[serde(rename = "response.audio.delta")]
    ResponseAudioDelta {
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
        delta: String,
    },

    #[serde(rename = "response.audio.done")]
    ResponseAudioDone {
        event_id: String,
        response_id: String,
        item_id: String,
        output_index: u32,
        content_index: u32,
    },

    #[serde(rename = "response.output_item.added")]
    ResponseOutputItemAdded {
        event_id: String,
        response_id: String,
        output_index: u32,
        item: serde_json::Value,
    },

    #[serde(rename = "response.output_item.done")]
    ResponseOutputItemDone {
        event_id: String,
        response_id: String,
        output_index: u32,
        item: serde_json::Value,
    },

    #[serde(rename = "response.done")]
    ResponseDone { event_id: String, response: serde_json::Value },

    #[serde(rename = "conversation.item.truncated")]
    ConversationItemTruncated {
        event_id: String,
        item_id: String,
        content_index: u32,
        audio_end_ms: u32,
    },

    #[serde(rename = "output_audio_buffer.started")]
    OutputAudioBufferStarted { event_id: String, response_id: String },

    #[serde(rename = "output_audio_buffer.stopped")]
    OutputAudioBufferStopped { event_id: String, response_id: String },

    #[serde(rename = "error.event")]
    ErrorEvent { event_id: String, code: u16, message: String },

    #[serde(rename = "output_audio_buffer.cleared")]
    OutputAudioBufferCleared { event_id: String, response_id: String },

    #[serde(rename = "conversation.item.input_audio_transcription.failed")]
    AsrTranscriptionFailed {
        event_id: String,
        item_id: String,
        content_index: u32,
        error: serde_json::Value,
    },

    #[serde(rename = "response.function_call_arguments.delta")]
    ResponseFunctionCallArgumentsDelta {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        delta: String,
    },

    #[serde(rename = "response.function_call_arguments.done")]
    ResponseFunctionCallArgumentsDone {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        function_name: String,
        arguments: String,
    },

    /// 新增：session.update 事件用于会话配置更新
    #[serde(rename = "session.update")]
    SessionUpdate { event_id: String, session: serde_json::Value },

    /// 新增：response.cancel 事件用于取消当前响应
    #[serde(rename = "response.cancel")]
    ResponseCancel { event_id: String, response_id: String },

    // 保持向后兼容的旧版本事件
    #[serde(rename = "response.function_call.delta")]
    ResponseFunctionCallDelta {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        delta: String,
    },

    #[serde(rename = "response.function_call.done")]
    ResponseFunctionCallDone {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        arguments: String,
    },

    #[serde(rename = "response.function_call_result.delta")]
    ResponseFunctionCallResultDelta {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        delta: String,
    },

    #[serde(rename = "response.function_call_result.done")]
    ResponseFunctionCallResultDone {
        event_id: String,
        response_id: String,
        item_id: String,
        call_id: String,
        result: String,
    },

    #[serde(rename = "response.language.detected")]
    ResponseLanguageDetected { event_id: String, response_id: String, code: String },

    #[serde(other)]
    Unknown,
}

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("JSON解析错误: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Base64解码错误: {0}")]
    Base64Error(String),

    #[error("无效的会话ID: {0}")]
    InvalidSessionId(String),

    #[error("二进制协议错误: {0}")]
    BinaryError(String),

    #[error("无效的头部大小: 期望 {expected}, 实际 {actual}")]
    InvalidHeaderSize { expected: usize, actual: usize },

    #[error("无效的nanoid长度: {0}")]
    InvalidNanoidLength(usize),
}

impl BinaryHeader {
    /// 创建新的二进制头部
    pub fn new(session_id: String, protocol_id: ProtocolId, command_id: CommandId) -> Result<Self, ProtocolError> {
        if session_id.len() != NANOID_SIZE {
            return Err(ProtocolError::InvalidNanoidLength(session_id.len()));
        }

        Ok(Self { session_id, protocol_id, command_id, reserved: [0; 14] })
    }

    /// 生成新的nanoid session ID
    pub fn generate_session_id() -> String {
        nanoid!(NANOID_SIZE)
    }

    /// 从字节数组解析头部
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < BINARY_HEADER_SIZE {
            return Err(ProtocolError::InvalidHeaderSize { expected: BINARY_HEADER_SIZE, actual: bytes.len() });
        }

        // 提取nanoid (16字节)
        let session_id = String::from_utf8(bytes[0..NANOID_SIZE].to_vec()).map_err(|e| ProtocolError::BinaryError(format!("无效的UTF-8 nanoid: {}", e)))?;

        // 提取业务ID (1字节)
        let protocol_id_raw = bytes[PROTOCOL_ID_OFFSET];
        let protocol_id = ProtocolId::from_u8(protocol_id_raw).ok_or_else(|| ProtocolError::BinaryError(format!("无效的协议ID: {}", protocol_id_raw)))?;

        // 提取命令ID (1字节)
        let command_id_raw = bytes[COMMAND_ID_OFFSET];
        let command_id = CommandId::from_u8(command_id_raw).ok_or_else(|| ProtocolError::BinaryError(format!("无效的命令ID: {}", command_id_raw)))?;

        // 解析 reserved 字段 (bytes[18..32])
        let mut reserved = [0u8; 14];
        reserved.copy_from_slice(&bytes[COMMAND_ID_OFFSET + 1..BINARY_HEADER_SIZE]);

        Ok(Self { session_id, protocol_id, command_id, reserved })
    }

    /// 转换为字节数组
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.session_id.len() != NANOID_SIZE {
            return Err(ProtocolError::InvalidNanoidLength(self.session_id.len()));
        }

        let mut bytes = vec![0u8; BINARY_HEADER_SIZE];

        // 写入nanoid
        bytes[0..NANOID_SIZE].copy_from_slice(self.session_id.as_bytes());

        // 写入业务ID
        bytes[PROTOCOL_ID_OFFSET] = self.protocol_id.as_u8();

        // 写入命令ID
        bytes[COMMAND_ID_OFFSET] = self.command_id.as_u8();

        // 写入 reserved
        bytes[COMMAND_ID_OFFSET + 1..BINARY_HEADER_SIZE].copy_from_slice(&self.reserved);

        Ok(bytes)
    }
}

impl BinaryMessage {
    /// 创建新的二进制消息
    pub fn new(header: BinaryHeader, payload: Vec<u8>) -> Result<Self, ProtocolError> {
        Ok(Self { header, payload })
    }

    /// 创建ASR音频数据消息
    pub fn asr_audio_chunk(session_id: String, audio_data: &[f32]) -> Result<Self, ProtocolError> {
        let payload = audio_data.iter().flat_map(|f| f.to_le_bytes()).collect();
        let header = BinaryHeader::new(session_id, ProtocolId::Asr, CommandId::AudioChunk)?;
        Self::new(header, payload)
    }

    /// 创建ASR开始会话消息
    pub fn asr_start_session(session_id: String) -> Result<Self, ProtocolError> {
        let header = BinaryHeader::new(session_id, ProtocolId::Asr, CommandId::Start)?;
        Self::new(header, vec![])
    }

    /// 创建ASR停止会话消息
    pub fn asr_stop_session(session_id: String) -> Result<Self, ProtocolError> {
        let header = BinaryHeader::new(session_id, ProtocolId::Asr, CommandId::Stop)?;
        Self::new(header, vec![])
    }

    /// 🆕 创建视觉图像数据消息（包含用户提示词）
    /// payload格式: [prompt_length(4bytes)] + [prompt_utf8_bytes] + [image_data]
    pub fn vision_image_data(session_id: String, user_prompt: &str, image_data: &[u8]) -> Result<Self, ProtocolError> {
        let prompt_bytes = user_prompt.as_bytes();
        let prompt_len = prompt_bytes.len() as u32;

        let mut payload = Vec::new();
        payload.extend_from_slice(&prompt_len.to_le_bytes()); // 4字节的提示词长度
        payload.extend_from_slice(prompt_bytes); // UTF-8编码的提示词
        payload.extend_from_slice(image_data); // 图像数据

        let header = BinaryHeader::new(session_id, ProtocolId::All, CommandId::ImageData)?;
        Self::new(header, payload)
    }

    /// 🆕 创建响应音频增量消息（二进制格式）
    /// payload格式: [response_id_len(4bytes)] + [response_id_utf8] + [item_id_len(4bytes)] + [item_id_utf8] +
    ///              [output_index(4bytes)] + [content_index(4bytes)] + [audio_data]
    pub fn response_audio_delta(session_id: String, response_id: &str, item_id: &str, output_index: u32, content_index: u32, audio_data: &[u8]) -> Result<Self, ProtocolError> {
        let response_id_bytes = response_id.as_bytes();
        let response_id_len = response_id_bytes.len() as u32;

        let item_id_bytes = item_id.as_bytes();
        let item_id_len = item_id_bytes.len() as u32;

        let mut payload = Vec::new();

        // 写入response_id长度和内容
        payload.extend_from_slice(&response_id_len.to_le_bytes());
        payload.extend_from_slice(response_id_bytes);

        // 写入item_id长度和内容
        payload.extend_from_slice(&item_id_len.to_le_bytes());
        payload.extend_from_slice(item_id_bytes);

        // 写入output_index和content_index
        payload.extend_from_slice(&output_index.to_le_bytes());
        payload.extend_from_slice(&content_index.to_le_bytes());

        // 写入音频数据
        payload.extend_from_slice(audio_data);

        let header = BinaryHeader::new(session_id, ProtocolId::All, CommandId::ResponseAudioDelta)?;
        Self::new(header, payload)
    }

    /// 从字节数组解析完整消息
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < BINARY_HEADER_SIZE {
            return Err(ProtocolError::InvalidHeaderSize { expected: BINARY_HEADER_SIZE, actual: bytes.len() });
        }

        let header = BinaryHeader::from_bytes(&bytes[0..BINARY_HEADER_SIZE])?;
        let payload = bytes[BINARY_HEADER_SIZE..].to_vec();

        Ok(Self { header, payload })
    }

    /// 转换为字节数组
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut bytes = self.header.to_bytes()?;
        bytes.extend_from_slice(&self.payload);
        Ok(bytes)
    }

    /// 获取会话ID
    pub fn session_id(&self) -> &str {
        &self.header.session_id
    }

    /// 判断是否为音频数据消息
    pub fn is_audio_chunk(&self) -> bool {
        self.header.protocol_id == ProtocolId::Asr && self.header.command_id == CommandId::AudioChunk
    }

    /// 判断是否为开始会话消息
    pub fn is_start_session(&self) -> bool {
        self.header.command_id == CommandId::Start
    }

    /// 判断是否为停止会话消息
    pub fn is_stop_session(&self) -> bool {
        self.header.command_id == CommandId::Stop
    }

    /// 🆕 解析视觉图像数据消息，返回 (用户提示词, 图像数据)
    /// 支持新格式: [prompt_length(4bytes)] + [prompt_utf8_bytes] + [image_data]
    /// 兼容旧格式: 纯图像数据，返回空提示词
    pub fn parse_vision_image_data(&self) -> Result<(String, Vec<u8>), ProtocolError> {
        if self.header.command_id != CommandId::ImageData {
            return Err(ProtocolError::BinaryError("不是ImageData消息".to_string()));
        }

        if self.payload.len() < 4 {
            // 兼容旧格式：纯图像数据
            return Ok((String::new(), self.payload.clone()));
        }

        // 尝试解析新格式
        let prompt_len = u32::from_le_bytes([self.payload[0], self.payload[1], self.payload[2], self.payload[3]]) as usize;

        // 验证长度合理性
        if prompt_len > self.payload.len() - 4 {
            // 可能是旧格式，返回纯图像数据
            return Ok((String::new(), self.payload.clone()));
        }

        // 如果提示词长度为0，说明没有提示词，但使用新格式
        if prompt_len == 0 {
            let image_data = self.payload[4..].to_vec();
            return Ok((String::new(), image_data));
        }

        // 解析提示词
        let prompt_end = 4 + prompt_len;
        if prompt_end > self.payload.len() {
            return Err(ProtocolError::BinaryError("提示词长度超出payload范围".to_string()));
        }

        let prompt_bytes = &self.payload[4..prompt_end];
        let user_prompt = String::from_utf8(prompt_bytes.to_vec()).map_err(|e| ProtocolError::BinaryError(format!("提示词UTF-8解码失败: {}", e)))?;

        // 解析图像数据
        let image_data = self.payload[prompt_end..].to_vec();

        Ok((user_prompt, image_data))
    }

    /// 🆕 解析响应音频增量消息，返回 (response_id, item_id, output_index, content_index, audio_data)
    pub fn parse_response_audio_delta(&self) -> Result<(String, String, u32, u32, Vec<u8>), ProtocolError> {
        if self.header.command_id != CommandId::ResponseAudioDelta {
            return Err(ProtocolError::BinaryError("不是ResponseAudioDelta消息".to_string()));
        }

        // 至少包含: 4(response_id_len) + 0(response_id) + 4(item_id_len) + 0(item_id) + 4(output_index) + 4(content_index)
        if self.payload.len() < 16 {
            return Err(ProtocolError::BinaryError("响应音频delta消息载荷过短".to_string()));
        }

        let mut offset = 0;

        // 解析response_id
        let response_id_len = u32::from_le_bytes([
            self.payload[offset],
            self.payload[offset + 1],
            self.payload[offset + 2],
            self.payload[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + response_id_len > self.payload.len() {
            return Err(ProtocolError::BinaryError("response_id长度超出payload范围".to_string()));
        }

        let response_id = String::from_utf8(self.payload[offset..offset + response_id_len].to_vec()).map_err(|e| ProtocolError::BinaryError(format!("response_id UTF-8解码失败: {}", e)))?;
        offset += response_id_len;

        // 解析item_id
        if offset + 4 > self.payload.len() {
            return Err(ProtocolError::BinaryError("item_id长度字段超出payload范围".to_string()));
        }

        let item_id_len = u32::from_le_bytes([
            self.payload[offset],
            self.payload[offset + 1],
            self.payload[offset + 2],
            self.payload[offset + 3],
        ]) as usize;
        offset += 4;

        if offset + item_id_len > self.payload.len() {
            return Err(ProtocolError::BinaryError("item_id长度超出payload范围".to_string()));
        }

        let item_id = String::from_utf8(self.payload[offset..offset + item_id_len].to_vec()).map_err(|e| ProtocolError::BinaryError(format!("item_id UTF-8解码失败: {}", e)))?;
        offset += item_id_len;

        // 解析output_index和content_index
        if offset + 8 > self.payload.len() {
            return Err(ProtocolError::BinaryError("索引字段超出payload范围".to_string()));
        }

        let output_index = u32::from_le_bytes([
            self.payload[offset],
            self.payload[offset + 1],
            self.payload[offset + 2],
            self.payload[offset + 3],
        ]);
        offset += 4;

        let content_index = u32::from_le_bytes([
            self.payload[offset],
            self.payload[offset + 1],
            self.payload[offset + 2],
            self.payload[offset + 3],
        ]);
        offset += 4;

        // 解析音频数据
        let audio_data = self.payload[offset..].to_vec();

        Ok((response_id, item_id, output_index, content_index, audio_data))
    }
}

impl WebSocketMessage {
    /// 创建新的WebSocket消息
    pub fn new(protocol_id: ProtocolId, command_id: CommandId, session_id: String, payload: Option<MessagePayload>) -> Self {
        Self { protocol_id, command_id, session_id, payload, timezone: None, location: None }
    }
    /// 序列化为JSON字符串
    pub fn to_json(&self) -> Result<String, ProtocolError> {
        serde_json::to_string(self).map_err(ProtocolError::from)
    }

    /// 从JSON字符串解析
    pub fn from_json(json: &str) -> Result<Self, ProtocolError> {
        serde_json::from_str(json).map_err(ProtocolError::from)
    }

    /// 从JSON字符串安全地解析WebSocket消息
    /// 即使payload解析失败，也尝试返回基础消息
    pub fn from_json_safe(json_str: &str) -> Result<Self, ProtocolError> {
        // 直接解析；若payload导致失败，则尝试只解析基础字段
        let result: serde_json::Result<Self> = serde_json::from_str(json_str);
        match result {
            Ok(msg) => Ok(msg),
            Err(e) => {
                if e.is_data() || e.to_string().contains("payload") {
                    tracing::warn!("⚠️ WebSocket JSON解析失败，payload将被置空进行回退: {}", e);
                    tracing::warn!("🔍 解析失败的JSON内容: {}", json_str);

                    // 尝试保留原始payload：若payload是对象但缺少type，则补上"session_config"后重试完整反序列化
                    if let Ok(mut root) = serde_json::from_str::<serde_json::Value>(json_str)
                        && let Some(obj) = root.as_object_mut()
                        && let Some(payload_val) = obj.get_mut("payload")
                        && let Some(pmap) = payload_val.as_object_mut()
                    {
                        let ty = pmap.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if ty.is_empty() {
                            pmap.insert("type".to_string(), serde_json::Value::String("session_config".to_string()));
                            if let Ok(msg2) = serde_json::from_value::<Self>(root.clone()) {
                                return Ok(msg2);
                            }
                        }
                    }

                    #[derive(Deserialize)]
                    struct TempMsg {
                        protocol_id: ProtocolId,
                        command_id: CommandId,
                        session_id: String,
                        #[serde(default)]
                        timezone: Option<String>,
                        #[serde(default)]
                        location: Option<String>,
                    }
                    if let Ok(temp_msg) = serde_json::from_str::<TempMsg>(json_str) {
                        return Ok(Self {
                            protocol_id: temp_msg.protocol_id,
                            command_id: temp_msg.command_id,
                            session_id: temp_msg.session_id,
                            payload: None,
                            timezone: temp_msg.timezone,
                            location: temp_msg.location,
                        });
                    }
                }
                Err(ProtocolError::from(e))
            },
        }
    }

    /// 转换为二进制消息
    pub fn to_binary(&self) -> Result<BinaryMessage, ProtocolError> {
        let payload = match &self.payload {
            Some(MessagePayload::AudioChunk { data, .. }) => {
                use base64::{Engine as _, engine::general_purpose};
                general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| ProtocolError::Base64Error(e.to_string()))?
            },
            Some(MessagePayload::ResponseAudioDelta { response_id, item_id, output_index, content_index, delta }) => {
                // 将ResponseAudioDelta转换为二进制格式
                use base64::{Engine as _, engine::general_purpose};
                let audio_data = general_purpose::STANDARD
                    .decode(delta)
                    .map_err(|e| ProtocolError::Base64Error(e.to_string()))?;

                // 创建二进制载荷
                let response_id_bytes = response_id.as_bytes();
                let response_id_len = response_id_bytes.len() as u32;

                let item_id_bytes = item_id.as_bytes();
                let item_id_len = item_id_bytes.len() as u32;

                let mut binary_payload = Vec::new();

                // 写入response_id长度和内容
                binary_payload.extend_from_slice(&response_id_len.to_le_bytes());
                binary_payload.extend_from_slice(response_id_bytes);

                // 写入item_id长度和内容
                binary_payload.extend_from_slice(&item_id_len.to_le_bytes());
                binary_payload.extend_from_slice(item_id_bytes);

                // 写入output_index和content_index
                binary_payload.extend_from_slice(&output_index.to_le_bytes());
                binary_payload.extend_from_slice(&content_index.to_le_bytes());

                // 写入音频数据
                binary_payload.extend_from_slice(&audio_data);

                binary_payload
            },
            _ => vec![], // 其他Realtime API事件都作为JSON传输，不需要二进制载荷
        };
        let header = BinaryHeader::new(self.session_id.clone(), self.protocol_id, self.command_id)?;
        BinaryMessage::new(header, payload)
    }
}

impl From<BinaryMessage> for WebSocketMessage {
    /// 从二进制消息转换为WebSocket消息
    fn from(binary_msg: BinaryMessage) -> Self {
        let payload: Option<MessagePayload> = match binary_msg.header.command_id {
            CommandId::Start => None,
            CommandId::Stop => None,
            CommandId::Interrupt => None,
            CommandId::AudioChunk => {
                use base64::{Engine as _, engine::general_purpose};
                let encoded_data = general_purpose::STANDARD.encode(&binary_msg.payload);
                Some(MessagePayload::AudioChunk {
                    data: encoded_data,
                    sample_rate: 16000, // 🔧 跟随TTS输出格式：16kHz
                    channels: 1,        // 🔧 跟随TTS输出格式：单声道
                })
            },
            CommandId::ImageData => {
                // 视觉输入走二进制通道，JSON事件不承载图像
                None
            },
            CommandId::ResponseAudioDelta => {
                // 解析响应音频delta二进制消息
                match binary_msg.parse_response_audio_delta() {
                    Ok((response_id, item_id, output_index, content_index, audio_data)) => {
                        use base64::{Engine as _, engine::general_purpose};
                        let delta_b64 = general_purpose::STANDARD.encode(&audio_data);
                        Some(MessagePayload::ResponseAudioDelta { response_id, item_id, output_index, content_index, delta: delta_b64 })
                    },
                    Err(_) => {
                        // 解析失败，返回错误事件
                        let event_id = format!("event_{}", nanoid!(8));
                        Some(MessagePayload::ErrorEvent { event_id, message: "响应音频delta解析失败".to_string(), code: 400 })
                    },
                }
            },
            CommandId::Result => {
                // Realtime API事件通过JSON传输，二进制消息不包含事件载荷
                None
            },
            _ => {
                // 未知命令，返回错误事件
                let event_id = format!("event_{}", nanoid!(8));
                Some(MessagePayload::ErrorEvent { event_id, message: "未知的命令ID".to_string(), code: 400 })
            },
        };

        Self {
            protocol_id: binary_msg.header.protocol_id,
            command_id: binary_msg.header.command_id,
            session_id: binary_msg.header.session_id,
            payload,
            timezone: None,
            location: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::realtime_event::create_response_audio_delta_binary_message;
    use serde_json;

    /// 生成16位测试用的session_id
    fn generate_test_session_id() -> String {
        "1234567890123456".to_string()
    }

    #[test]
    fn test_tts_audio_delta_binary_message() {
        let session_id = generate_test_session_id();
        let response_id = "resp_456";
        let item_id = "item_789";
        let output_index = 1u32;
        let content_index = 0u32;
        let audio_data = vec![0x01, 0x02, 0x03, 0x04, 0x05];

        let binary_msg = create_response_audio_delta_binary_message(
            session_id.clone(),
            response_id,
            item_id,
            output_index,
            content_index,
            &audio_data,
        )
        .expect("创建二进制消息失败");

        assert_eq!(binary_msg.header.session_id, session_id);
        assert_eq!(binary_msg.header.protocol_id, ProtocolId::All);
        assert_eq!(binary_msg.header.command_id, CommandId::ResponseAudioDelta);

        let (parsed_response_id, parsed_item_id, parsed_output_index, parsed_content_index, parsed_audio_data) = binary_msg.parse_response_audio_delta().expect("解析二进制消息失败");

        assert_eq!(parsed_response_id, response_id);
        assert_eq!(parsed_item_id, item_id);
        assert_eq!(parsed_output_index, output_index);
        assert_eq!(parsed_content_index, content_index);
        assert_eq!(parsed_audio_data, audio_data);
    }

    #[test]
    fn test_binary_message_roundtrip() {
        let session_id = generate_test_session_id();
        let response_id = "resp_roundtrip";
        let item_id = "item_roundtrip";
        let output_index = 42u32;
        let content_index = 10u32;
        let audio_data = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let original_msg = create_response_audio_delta_binary_message(
            session_id.clone(),
            response_id,
            item_id,
            output_index,
            content_index,
            &audio_data,
        )
        .expect("创建二进制消息失败");

        let bytes = original_msg.to_bytes().expect("转换为字节数组失败");
        let parsed_msg = BinaryMessage::from_bytes(&bytes).expect("从字节数组解析失败");

        let (parsed_response_id, parsed_item_id, parsed_output_index, parsed_content_index, parsed_audio_data) = parsed_msg.parse_response_audio_delta().expect("解析二进制消息失败");

        assert_eq!(parsed_response_id, response_id);
        assert_eq!(parsed_item_id, item_id);
        assert_eq!(parsed_output_index, output_index);
        assert_eq!(parsed_content_index, content_index);
        assert_eq!(parsed_audio_data, audio_data);
    }

    #[test]
    fn test_asr_transcription_failed_serialization() {
        use crate::rpc::realtime_event::create_asr_transcription_failed_message;

        let session_id = "test_session_123".to_string();
        let item_id = "msg_test_456";
        let content_index = 0;
        let code = "no_output";
        let message = "检测到打断后无有效语音输入，ASR转录失败";

        let msg = create_asr_transcription_failed_message(session_id, item_id, content_index, code, message);
        let json_str = msg.to_json().expect("序列化失败");

        assert!(json_str.contains("\"type\":\"conversation.item.input_audio_transcription.failed\""));
        assert!(json_str.contains("\"item_id\":\"msg_test_456\""));
        assert!(json_str.contains("\"content_index\":0"));
        assert!(json_str.contains("\"code\":\"no_output\""));
        assert!(json_str.contains("\"message\":\"检测到打断后无有效语音输入，ASR转录失败\""));
    }

    #[test]
    fn test_websocket_message_empty_payload() {
        let json = r#"{"protocol_id":100,"command_id":2,"session_id":"AI9myot94lOEZffQ","payload":{}}"#;
        let msg: WebSocketMessage = serde_json::from_str(json).expect("Should parse empty payload");
        assert!(msg.payload.is_none(), "Payload should be None when empty object is provided");
    }
}
