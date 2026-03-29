//! 音频处理模块
//!
//! 负责音频数据的保存和处理
//! 目前主要功能：音频分段保存
//!
//! ## 音频格式标准化政策
//!
//! ### 内部音频格式
//! - **标准格式**: f32 归一化音频，范围 [-1.0, 1.0]
//! - **采样率**: 16000 Hz
//! - **声道**: 单声道 (mono)
//! - **字节序**: 小端序 (little-endian)
//!
//! ### 外部接口格式
//! - **输入格式**: s16le PCM（±32767 范围）
//! - **输出格式**: 根据目标组件需求转换
//!
//! ### 转换规则
//! ```rust
//! // 从 i16 转换到归一化 f32
//! let normalized = sample_i16 as f32 / i16::MAX as f32;
//!
//! // 从归一化 f32 转换到 i16
//! let sample_i16 = (normalized * 32767.0).clamp(-32768.0, 32767.0) as i16;
//!
//! // 转换到 Kaldi 格式（用于 ASR）
//! let kaldi_sample = normalized * 32768.0;
//! ```
//!
//! ### 组件音频格式要求
//! - **VAD**: 归一化 f32 [-1.0, 1.0] ✓
//! - **ASR**: 内部转换为 Kaldi 格式 (×32768.0) ✓
//! - **TTS**: 输出时转换为 i16 PCM ✓

// use std::i16; // Legacy numeric constants - removed as suggested by clippy
// 删除 use serde::{Serialize, Deserialize} 相关注释和内容。

pub mod denoiser;
pub mod input_processor;
pub mod opus_proc;
pub mod tts_frame;

// 重新导出核心类型
pub use opus_proc::{OpusDecoder, OpusDecoderConfig, OpusEncoder, OpusEncoderConfig, OpusError, OpusFrame};

// 新的配置结构体将在下面定义并自动导出
pub use input_processor::{AudioInputConfig, AudioInputProcessor, InputProcessorError};

// TTS音频帧处理
pub use tts_frame::{TTS_OUTPUT_SAMPLE_RATE, TTS_SOURCE_SAMPLE_RATE, TtsAudioFrame};

/// 音频编码格式定义（简化版，不包含配置参数）
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    /// 16-bit PCM (小端序)
    #[default]
    #[serde(alias = "pcm", alias = "pcm_s16_le", alias = "PcmS16Le", alias = "pcm_s16le")]
    PcmS16Le,
    /// 24-bit PCM (小端序)
    #[serde(alias = "pcm_s24_le", alias = "PcmS24Le", alias = "pcm_s24le")]
    PcmS24Le,
    /// 32-bit PCM (小端序)
    #[serde(alias = "pcm_s32_le", alias = "PcmS32Le", alias = "pcm_s32le")]
    PcmS32Le,
    /// Opus 编码格式
    #[serde(alias = "opus", alias = "Opus")]
    Opus,
    /// 其他格式，需要指定每样本字节数
    Other(u32),
}

/// 音频输出配置（包含格式、时间片、编码参数等）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputAudioConfig {
    /// 音频格式
    #[serde(default = "AudioFormat::default")]
    pub format: AudioFormat,
    /// 音频时间片长度（毫秒）
    #[serde(default = "OutputAudioConfig::default_slice_ms")]
    pub slice_ms: u32,
    /// Opus 编码配置（当 format == Opus 时使用）
    #[serde(default)]
    pub opus_config: Option<OpusEncoderConfig>,
}

impl AudioFormat {
    /// 创建默认的PCM格式
    pub fn default_pcm() -> Self {
        AudioFormat::PcmS16Le
    }

    /// 创建默认的Opus格式
    pub fn default_opus() -> Self {
        AudioFormat::Opus
    }

    /// 从字符串解析音频格式
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "pcm" => AudioFormat::PcmS16Le,
            "opus" => AudioFormat::Opus,
            _ => AudioFormat::PcmS16Le, // 默认PCM
        }
    }

    /// 验证Opus帧时长是否符合标准
    pub fn validate_opus_frame_duration(duration_ms: u32) -> bool {
        matches!(duration_ms, 5 | 10 | 20 | 40 | 60)
    }

    /// 获取Opus标准帧时长列表
    pub fn opus_standard_frame_durations() -> Vec<u32> {
        vec![5, 10, 20, 40, 60]
    }

    /// 为Opus格式选择最接近的标准帧时长
    pub fn closest_opus_frame_duration(target_ms: u32) -> u32 {
        let standards = Self::opus_standard_frame_durations();
        let mut closest = standards[0];
        let mut min_diff = (target_ms as i32 - closest as i32).abs();

        for &duration in &standards {
            let diff = (target_ms as i32 - duration as i32).abs();
            if diff < min_diff {
                min_diff = diff;
                closest = duration;
            }
        }
        closest
    }

    /// 获取每样本字节数（仅适用于PCM格式）
    pub fn bytes_per_sample(&self) -> u32 {
        match self {
            AudioFormat::PcmS16Le => 2,
            AudioFormat::PcmS24Le => 3,
            AudioFormat::PcmS32Le => 4,
            AudioFormat::Opus => 2, // Opus使用16位PCM作为输入
            AudioFormat::Other(bytes) => *bytes,
        }
    }

    /// 检测音频格式是否为PCM
    pub fn is_pcm(&self) -> bool {
        matches!(self, AudioFormat::PcmS16Le | AudioFormat::PcmS24Le | AudioFormat::PcmS32Le)
    }

    /// 检测音频格式是否为Opus
    pub fn is_opus(&self) -> bool {
        matches!(self, AudioFormat::Opus)
    }
}

impl Default for OutputAudioConfig {
    fn default() -> Self {
        Self { format: AudioFormat::PcmS16Le, slice_ms: 20, opus_config: None }
    }
}

impl OutputAudioConfig {
    #[inline]
    pub fn default_slice_ms() -> u32 {
        20
    }

    /// 创建默认PCM输出配置
    pub fn default_pcm(slice_ms: u32) -> Self {
        Self { format: AudioFormat::PcmS16Le, slice_ms, opus_config: None }
    }

    /// 创建Opus输出配置
    pub fn opus(slice_ms: u32, opus_config: OpusEncoderConfig) -> Self {
        Self { format: AudioFormat::Opus, slice_ms, opus_config: Some(opus_config) }
    }

    /// 创建默认Opus输出配置
    pub fn default_opus(slice_ms: u32) -> Self {
        let opus_config = OpusEncoderConfig { frame_duration_ms: Some(slice_ms), ..Default::default() };
        Self { format: AudioFormat::Opus, slice_ms, opus_config: Some(opus_config) }
    }

    /// 检查配置是否有效
    pub fn validate(&self) -> Result<(), String> {
        if self.slice_ms == 0 {
            return Err("slice_ms 不能为0".to_string());
        }

        // 对于Opus格式，验证帧时长
        if self.format.is_opus() {
            if !AudioFormat::validate_opus_frame_duration(self.slice_ms) {
                return Err(format!(
                    "Opus帧时长 {}ms 不符合标准，必须是 5/10/20/40/60 ms 之一",
                    self.slice_ms
                ));
            }

            // 确保opus_config中的frame_duration_ms与slice_ms一致
            if let Some(ref opus_config) = self.opus_config
                && let Some(frame_duration) = opus_config.frame_duration_ms
                && frame_duration != self.slice_ms
            {
                return Err(format!(
                    "opus_config.frame_duration_ms ({}) 与 slice_ms ({}) 不一致",
                    frame_duration, self.slice_ms
                ));
            }
        }

        Ok(())
    }

    /// 自动纠正配置（主要是Opus帧时长）
    pub fn auto_correct(&mut self) {
        if self.format.is_opus() {
            if !AudioFormat::validate_opus_frame_duration(self.slice_ms) {
                let corrected = AudioFormat::closest_opus_frame_duration(self.slice_ms);
                self.slice_ms = corrected;
                if let Some(ref mut opus_config) = self.opus_config {
                    opus_config.frame_duration_ms = Some(corrected);
                } else {
                    self.opus_config = Some(OpusEncoderConfig { frame_duration_ms: Some(corrected), ..Default::default() });
                }
            } else if let Some(ref mut opus_config) = self.opus_config {
                opus_config.frame_duration_ms = Some(self.slice_ms);
            } else {
                self.opus_config = Some(OpusEncoderConfig { frame_duration_ms: Some(self.slice_ms), ..Default::default() });
            }
        }
    }
}

/// 音频统计信息
#[derive(Debug, Clone)]
pub struct AudioStats {
    pub total_frames_processed: u64,
    pub total_bytes_processed: u64,
    pub average_latency_ms: f32,
    pub current_buffer_usage: f32,
}

/// 音频处理错误
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("IO错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("配置错误: {0}")]
    Config(String),

    #[error("解码错误: {0}")]
    Decode(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("缓冲区错误: {0}")]
    Buffer(String),

    #[error("其他错误: {0}")]
    Other(String),
}

pub fn bytes_to_f32_vec(bytes: &[u8]) -> Result<(Vec<f32>, usize), String> {
    if !bytes.len().is_multiple_of(2) {
        let error_msg = format!("音频字节长度不是2的倍数: {}，丢弃不完整的帧", bytes.len());
        tracing::error!("{}", error_msg);
        return Err(error_msg);
    }

    // 🔧 优化：预分配容量，避免动态扩容
    let sample_count = bytes.len() / 2;
    let mut result = Vec::with_capacity(sample_count);

    // 直接将 &[u8] 重解释为 &[i16]，然后转换为 Vec<f32>
    let i16_slice = unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const i16, sample_count) };

    result.extend(i16_slice.iter().map(|&x| i16::from_le(x) as f32 / (i16::MAX as f32)));

    Ok((result, sample_count))
}
