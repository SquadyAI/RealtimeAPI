//! 高性能Opus编码器模块
//! 提供无状态、零拷贝的Opus音频编码功能
//! 支持实时音频流编码，优化延迟和性能

use bytes::Bytes;
use opus::{Application, Bandwidth, Channels, Signal};
use thiserror::Error;
use tracing::debug;

/// Opus编码器错误
#[derive(Debug, Error)]
pub enum OpusError {
    #[error("Opus编码错误: {0}")]
    Encode(String),

    #[error("Opus解码错误: {0}")]
    Decode(String),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("缓冲区错误: {0}")]
    Buffer(String),

    #[error("采样率不支持: {0}")]
    UnsupportedSampleRate(u32),

    #[error("声道数不支持: {0}")]
    UnsupportedChannels(u16),

    #[error("帧大小错误: {0}")]
    InvalidFrameSize(usize),

    #[error("其他错误: {0}")]
    Other(String),
}

/// Opus编码器配置 - 可序列化版本（不包含sample_rate和channels，由TTS输出决定）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(default)]
pub struct OpusEncoderConfig {
    /// 比特率 (bps)
    pub bitrate: u32,
    /// 帧时长 (毫秒)
    pub frame_duration_ms: Option<u32>,
    /// 编码复杂度 (0-10)
    pub complexity: u8,
    /// 应用类型 ("voip", "audio", "restricted_lowdelay")
    pub application: String,
    /// 是否启用可变比特率
    pub variable_bitrate: bool,
    /// 是否启用DTX (不连续传输)
    pub dtx: bool,
    /// 是否启用FEC (前向纠错)
    pub fec: bool,
    /// 最大带宽 ("narrowband", "mediumband", "wideband", "superwideband", "fullband")
    pub bandwidth: String,
    /// 信号类型 ("auto", "voice", "music")
    pub signal: String,
    /// 丢包率 (0-100)
    pub packet_loss_perc: u8,
}

/// 内部使用的Opus编码器配置 - 使用opus crate的类型
#[derive(Debug, Clone)]
pub struct InternalOpusEncoderConfig {
    /// 采样率 (Hz) - 由TTS输出决定
    pub sample_rate: u32,
    /// 声道数 - 由TTS输出决定
    pub channels: Channels,
    /// 比特率 (bps)
    pub bitrate: u32,
    /// 帧时长 (毫秒)
    pub frame_duration_ms: u32,
    /// 编码复杂度 (0-10)
    pub complexity: u8,
    /// 应用类型
    pub application: Application,
    /// 是否启用可变比特率
    pub variable_bitrate: bool,
    /// 是否启用DTX (不连续传输)
    pub dtx: bool,
    /// 是否启用FEC (前向纠错)
    pub fec: bool,
    /// 最大带宽
    pub bandwidth: Bandwidth,
    /// 信号类型
    pub signal: Signal,
    /// 丢包率
    pub packet_loss_perc: u8,
}

/// Opus解码器配置
#[derive(Debug, Clone)]
pub struct OpusDecoderConfig {
    /// 采样率 (Hz)
    pub sample_rate: u32,
    /// 声道数
    pub channels: Channels,
    /// 是否启用FEC (前向纠错)
    pub fec: bool,
    /// 是否启用PLC (丢包隐藏)
    pub plc: bool,
}

impl OpusDecoderConfig {
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let mut channels_d = Channels::Mono;
        if channels == 2 {
            channels_d = Channels::Stereo;
        }
        Self { sample_rate, channels: channels_d, ..Default::default() }
    }
}

impl Default for OpusDecoderConfig {
    fn default() -> Self {
        Self { sample_rate: 16000, channels: Channels::Mono, fec: false, plc: true }
    }
}

impl OpusEncoderConfig {
    pub fn new(frame_duration_ms: Option<u32>) -> Self {
        Self { frame_duration_ms, application: "audio".to_string(), ..Default::default() }
    }

    /// 转换为内部配置（使用opus crate的枚举类型），sample_rate和channels由TTS输出决定
    pub fn to_internal(&self, sample_rate: u32, channels: u16) -> Result<InternalOpusEncoderConfig, OpusError> {
        let channels_enum = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            _ => return Err(OpusError::UnsupportedChannels(channels)),
        };

        let application = match self.application.to_lowercase().as_str() {
            "voip" => Application::Voip,
            "audio" => Application::Audio,
            "lowdelay" => Application::LowDelay,
            _ => return Err(OpusError::Config(format!("不支持的应用类型: {}", self.application))),
        };

        let bandwidth = match self.bandwidth.to_lowercase().as_str() {
            "narrowband" => Bandwidth::Narrowband,
            "mediumband" => Bandwidth::Mediumband,
            "wideband" => Bandwidth::Wideband,
            "superwideband" => Bandwidth::Superwideband,
            "fullband" => Bandwidth::Fullband,
            _ => return Err(OpusError::Config(format!("不支持的带宽: {}", self.bandwidth))),
        };

        let signal = match self.signal.to_lowercase().as_str() {
            "auto" => Signal::Auto,
            "voice" => Signal::Voice,
            "music" => Signal::Music,
            _ => return Err(OpusError::Config(format!("不支持的信号类型: {}", self.signal))),
        };

        Ok(InternalOpusEncoderConfig {
            sample_rate,
            channels: channels_enum,
            bitrate: self.bitrate,
            frame_duration_ms: self.frame_duration_ms.unwrap_or(20),
            complexity: self.complexity,
            application,
            variable_bitrate: self.variable_bitrate,
            dtx: self.dtx,
            fec: self.fec,
            bandwidth,
            signal,
            packet_loss_perc: self.packet_loss_perc,
        })
    }
}

impl Default for OpusEncoderConfig {
    fn default() -> Self {
        Self {
            bitrate: 32000,                   // 32kbps
            frame_duration_ms: None,          // 20ms标准帧
            complexity: 2,                    // 固定复杂度2
            application: "audio".to_string(), // 语音优化
            variable_bitrate: false,          // 固定比特率（CBR）
            dtx: false,                       // 禁用静音检测
            fec: false,                       // 禁用前向纠错
            bandwidth: "fullband".to_string(),
            signal: "voice".to_string(),
            packet_loss_perc: 0,
        }
    }
}

impl Default for InternalOpusEncoderConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: Channels::Mono,
            bitrate: 32000,                  // 32kbps（复杂度2的最佳补偿点）
            frame_duration_ms: 20,           // 20ms标准帧
            complexity: 0,                   // 固定复杂度2
            application: Application::Audio, // 语音优化
            variable_bitrate: false,         // 固定比特率（CBR）
            dtx: false,                      // 禁用静音检测
            fec: false,                      // 禁用前向纠错
            bandwidth: Bandwidth::Fullband,
            signal: Signal::Voice,
            packet_loss_perc: 0,
        }
    }
}

/// Opus编码帧
#[derive(Debug, Clone)]
pub struct OpusFrame {
    /// 编码后的数据
    pub data: Bytes,
    /// 帧大小 (字节)
    pub size: usize,
}

/// 高性能Opus编码器
pub struct OpusEncoder {
    config: OpusEncoderConfig,
    encoder: opus::Encoder,
    frame_size: usize,
    output_buffer: Vec<u8>,
}

/// 高性能Opus解码器
pub struct OpusDecoder {
    config: OpusDecoderConfig,
    decoder: opus::Decoder,
    output_buffer: Vec<f32>,
    // last_frame: Option<Vec<f32>>, // 用于PLC
}

impl OpusDecoder {
    /// 创建新的Opus解码器
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self, OpusError> {
        let config = OpusDecoderConfig::new(sample_rate, channels);
        Self::with_config(config)
    }

    /// 使用自定义配置创建Opus解码器
    pub fn with_config(config: OpusDecoderConfig) -> Result<Self, OpusError> {
        // 创建Opus解码器
        let decoder = opus::Decoder::new(config.sample_rate, config.channels).map_err(|e| OpusError::Decode(format!("创建解码器失败: {}", e)))?;

        // 预分配更大的输出缓冲区，支持最大120ms帧
        // Opus最大解码帧大小为120ms * 采样率 * 声道数
        let max_frame_size = (config.sample_rate as usize * config.channels as usize * 120) / 1000;
        let output_buffer = vec![0.0f32; max_frame_size];

        Ok(Self {
            config,
            decoder,
            output_buffer,
            // 预分配 last_frame，用于PLC零拷贝复制，使用最大帧大小
            // last_frame: Some(vec![0.0f32; max_frame_size]),
        })
    }

    /// 解码Opus帧
    pub fn decode_frame(&mut self, opus_data: &[u8]) -> Result<Vec<f32>, OpusError> {
        // let start_time = std::time::Instant::now();

        // 检查输入数据大小
        if opus_data.is_empty() {
            return Err(OpusError::Decode("输入数据为空".to_string()));
        }

        // 检查数据大小是否合理（Opus帧通常在1-1275字节之间）
        if opus_data.len() > 1280 {
            return Err(OpusError::Decode(format!(
                "Opus帧过大: {}字节，最大支持1280字节",
                opus_data.len()
            )));
        }

        // 直接解码到已预分配的缓冲区
        let decoded_samples = self
            .decoder
            .decode_float(opus_data, &mut self.output_buffer, false)
            .map_err(|e| {
                // 提供更详细的错误信息
                OpusError::Decode(format!("解码失败: {:?} (数据大小: {}字节)", e, opus_data.len()))
            })?;

        // 检查解码结果是否合理
        if decoded_samples == 0 {
            return Err(OpusError::Decode("解码结果为空".to_string()));
        }

        if decoded_samples > self.output_buffer.len() {
            return Err(OpusError::Decode(format!(
                "解码样本数超出缓冲区大小: {} > {}",
                decoded_samples,
                self.output_buffer.len()
            )));
        }

        // 计算解码延迟
        // let _decode_latency_us = start_time.elapsed().as_micros() as u64;

        // 使用辅助方法创建解码帧
        let frame = self.create_decoded_frame(decoded_samples);

        // trace!("🎵 Opus解码完成: {}字节 -> {}样本 (延迟: {}μs)",
        //     opus_data.len(), decoded_samples, decode_latency_us);

        Ok(frame)
    }

    fn create_decoded_frame(&mut self, decoded_samples: usize) -> Vec<f32> {
        //根据decoded_samples从output_buffer中取出数据
        self.output_buffer[..decoded_samples].to_vec()
    }

    /// 获取配置
    pub fn get_config(&self) -> &OpusDecoderConfig {
        &self.config
    }

    /// 更新配置
    pub fn update_config(&mut self, config: OpusDecoderConfig) -> Result<(), OpusError> {
        // 如果关键参数发生变化，需要重新创建解码器
        if config.sample_rate != self.config.sample_rate || config.channels != self.config.channels {
            return Err(OpusError::Config("采样率或声道数变化需要重新创建解码器".to_string()));
        }

        self.config = config;
        debug!("🎛️ Opus解码器配置已更新");

        Ok(())
    }

    /// 重置解码器状态
    pub fn reset(&mut self) {
        // if let Some(ref mut last) = self.last_frame {
        //     last.fill(0.0);
        // }
        self.output_buffer.fill(0.0);

        // 确保缓冲区大小正确
        let max_frame_size = (self.config.sample_rate as usize * self.config.channels as usize * 120) / 1000;
        if self.output_buffer.len() != max_frame_size {
            self.output_buffer.resize(max_frame_size, 0.0);
        }
        // if let Some(ref mut last) = self.last_frame {
        //     if last.len() != max_frame_size {
        //         last.resize(max_frame_size, 0.0);
        //     }
        // }

        debug!("🔄 Opus解码器状态已重置，缓冲区大小: {}", max_frame_size);
    }
}

impl OpusEncoder {
    /// 创建新的Opus编码器
    pub fn new(sample_rate: u32, channels: u16, frame_duration_ms: Option<u32>) -> Result<Self, OpusError> {
        let config = OpusEncoderConfig::new(frame_duration_ms);
        Self::with_config(config, sample_rate, channels)
    }

    /// 使用自定义配置创建Opus编码器
    pub fn with_config(config: OpusEncoderConfig, sample_rate: u32, channels: u16) -> Result<Self, OpusError> {
        // 转换为内部配置
        let internal_config = config.to_internal(sample_rate, channels)?;

        // 计算帧大小
        // Opus 期待的 frame_size 是“每个声道的样本数”，不需要再乘以声道数。
        // 公式: samples_per_ms (sample_rate / 1000) * frame_duration_ms
        let frame_size = (internal_config.sample_rate as usize / 1000) * internal_config.frame_duration_ms as usize;

        // 创建Opus编码器
        let mut encoder = opus::Encoder::new(
            internal_config.sample_rate,
            internal_config.channels,
            internal_config.application,
        )
        .map_err(|e| OpusError::Encode(format!("创建编码器失败: {}", e)))?;

        // 设置编码器参数：确保从第一帧起即生效
        encoder
            .set_bitrate(opus::Bitrate::Bits(32000_i32))
            .map_err(|e| OpusError::Config(format!("设置比特率失败: {}", e)))?;
        encoder
            .set_complexity(0)
            .map_err(|e| OpusError::Config(format!("设置复杂度失败: {}", e)))?;
        encoder
            .set_vbr(false)
            .map_err(|e| OpusError::Config(format!("设置VBR失败: {}", e)))?;
        encoder
            .set_dtx(false)
            .map_err(|e| OpusError::Config(format!("设置DTX失败: {}", e)))?;
        encoder
            .set_inband_fec(false)
            .map_err(|e| OpusError::Config(format!("设置FEC失败: {}", e)))?;

        // 预分配固定长度缓冲，避免每帧clear/resize
        let output_buffer = vec![0u8; 1275]; // 最大Opus帧大小

        Ok(Self { config, encoder, frame_size, output_buffer })
    }

    /// 编码音频数据 (零拷贝版本)
    pub fn encode_frame(&mut self, audio_data: &[f32]) -> Result<OpusFrame, OpusError> {
        // let start_time = std::time::Instant::now();

        // 检查输入数据大小
        if audio_data.len() != self.frame_size {
            return Err(OpusError::InvalidFrameSize(audio_data.len()));
        }

        // 编码音频数据
        let encoded_size = self
            .encoder
            .encode_float(audio_data, &mut self.output_buffer)
            .map_err(|e| OpusError::Encode(format!("编码失败: {}", e)))?;

        // 计算编码延迟
        // let encode_latency_us = start_time.elapsed().as_micros() as u64;

        // 计算压缩比（按输入字节数）
        // let input_size_bytes = std::mem::size_of_val(audio_data);
        // let _compression_ratio = if encoded_size > 0 {
        //     input_size_bytes as f32 / encoded_size as f32
        // } else {
        //     1.0
        // };

        // 创建编码帧
        let frame = OpusFrame { data: self.output_buffer[..encoded_size].to_vec().into(), size: encoded_size };

        // trace!(
        //     "🎵 Opus编码完成: {}字节 -> {}字节 (压缩比: {:.2}x, 延迟: {}μs)",
        //     input_size_bytes, encoded_size, compression_ratio, encode_latency_us
        // );

        Ok(frame)
    }

    /// 获取配置
    pub fn get_config(&self) -> &OpusEncoderConfig {
        &self.config
    }

    /// 更新配置
    pub fn update_config(&mut self, config: OpusEncoderConfig) -> Result<(), OpusError> {
        // 更新可动态修改的参数
        self.encoder
            .set_bitrate(opus::Bitrate::Bits(32000_i32))
            .map_err(|e| OpusError::Config(format!("更新比特率失败: {}", e)))?;

        self.encoder
            .set_complexity(0)
            .map_err(|e| OpusError::Config(format!("更新复杂度失败: {}", e)))?;

        self.encoder
            .set_vbr(false)
            .map_err(|e| OpusError::Config(format!("更新VBR失败: {}", e)))?;

        self.encoder
            .set_dtx(false)
            .map_err(|e| OpusError::Config(format!("更新DTX失败: {}", e)))?;

        self.encoder
            .set_inband_fec(false)
            .map_err(|e| OpusError::Config(format!("更新FEC失败: {}", e)))?;

        self.config = config;
        debug!("🎛️ Opus编码器配置已更新");

        Ok(())
    }

    /// 获取帧大小
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// 获取帧时长 (毫秒)
    pub fn frame_duration_ms(&self) -> u32 {
        self.config.frame_duration_ms.unwrap_or(20)
    }
}
