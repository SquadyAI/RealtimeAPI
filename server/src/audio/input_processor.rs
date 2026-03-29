// src/audio/input_processor.rs
use crate::audio::{
    bytes_to_f32_vec,
    opus_proc::{OpusDecoder, OpusDecoderConfig, OpusError},
};
use tracing::warn;

/// 重采样函数 - 将音频从任意采样率重采样到目标采样率
fn resample_audio(input: &[f32], input_rate: u32, output_rate: u32) -> Result<Vec<f32>, InputProcessorError> {
    if input.is_empty() {
        return Ok(Vec::new());
    }

    if input_rate == 0 || output_rate == 0 {
        return Err(InputProcessorError::Other("采样率不能为0".to_string()));
    }

    // 如果采样率相同，直接返回
    if input_rate == output_rate {
        return Ok(input.to_vec());
    }

    let ratio = input_rate as f32 / output_rate as f32;
    let output_len = (input.len() as f32 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let input_pos = i as f32 * ratio;
        let input_index = input_pos.floor() as usize;
        let next_index = (input_index + 1).min(input.len() - 1);
        let fraction = input_pos - input_index as f32;

        let sample = if input_index < input.len() - 1 {
            input[input_index] * (1.0 - fraction) + input[next_index] * fraction
        } else {
            input[input_index]
        };

        if !sample.is_finite() {
            return Err(InputProcessorError::Other(format!("重采样产生无效样本: {}", sample)));
        }

        output.push(sample);
    }

    Ok(output)
}

/// 音频输入处理配置（客户端接口）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AudioInputConfig {
    /// 输入音频格式
    pub format: crate::audio::AudioFormat,
    /// 输入采样率 (Hz)
    pub sample_rate: u32,
}

impl Default for AudioInputConfig {
    fn default() -> Self {
        Self { format: crate::audio::AudioFormat::default_pcm(), sample_rate: 16000 }
    }
}

impl AudioInputConfig {
    /// 验证配置参数
    pub fn validate(&self) -> Result<(), String> {
        if self.sample_rate == 0 {
            return Err("sample_rate 不能为 0".to_string());
        }

        // 检查采样率是否在合理范围内
        if self.sample_rate < 8000 || self.sample_rate > 192000 {
            return Err(format!("sample_rate ({}) 超出支持范围 [8000, 192000]", self.sample_rate));
        }

        Ok(())
    }

    /// 自动纠正配置参数
    pub fn auto_correct(&mut self) {
        // 限制采样率在合理范围内
        self.sample_rate = self.sample_rate.clamp(8000, 192000);
    }

    /// 转换为内部处理配置
    pub fn to_internal_config(&self) -> InternalAudioInputConfig {
        InternalAudioInputConfig {
            format: self.format.clone(),
            input_sample_rate: self.sample_rate,
            target_sample_rate: 16000,                    // 内部固定使用16kHz
            enable_resampling: self.sample_rate != 16000, // 自动判断是否需要重采样
        }
    }
}

/// 内部音频输入处理配置
#[derive(Debug, Clone)]
pub struct InternalAudioInputConfig {
    /// 输入音频格式
    pub format: crate::audio::AudioFormat,
    /// 输入采样率 (Hz)
    pub input_sample_rate: u32,
    /// 目标采样率 (Hz) - 内部固定为16kHz
    pub target_sample_rate: u32,
    /// 是否启用自动重采样
    pub enable_resampling: bool,
}

/// 音频处理器 trait - 消除运行时分支
trait AudioProcessor: Send + Sync {
    fn process_audio_chunk(&mut self, data: &[u8]) -> Result<Vec<f32>, InputProcessorError>;
    fn reset(&mut self);
}

/// PCM 音频处理器 - 支持重采样
struct PcmProcessor {
    input_sample_rate: u32,
    target_sample_rate: u32,
    enable_resampling: bool,
}

impl PcmProcessor {
    fn new(config: &InternalAudioInputConfig) -> Self {
        Self {
            input_sample_rate: config.input_sample_rate,
            target_sample_rate: config.target_sample_rate,
            enable_resampling: config.enable_resampling,
        }
    }
}

impl AudioProcessor for PcmProcessor {
    fn process_audio_chunk(&mut self, data: &[u8]) -> Result<Vec<f32>, InputProcessorError> {
        // 对于PCM格式，直接转换为f32
        let (samples, _) = bytes_to_f32_vec(data).map_err(InputProcessorError::Convert)?;

        // 如果需要重采样且采样率不同
        if self.enable_resampling && self.input_sample_rate != self.target_sample_rate {
            resample_audio(&samples, self.input_sample_rate, self.target_sample_rate)
        } else {
            Ok(samples)
        }
    }

    fn reset(&mut self) {
        // PCM处理器不需要重置
    }
}

/// OPUS 音频处理器 - 支持重采样
struct OpusProcessor {
    decoder: std::sync::Mutex<OpusDecoder>,
    input_sample_rate: u32,
    target_sample_rate: u32,
    enable_resampling: bool,
}

impl OpusProcessor {
    fn new(config: InternalAudioInputConfig) -> Result<Self, InputProcessorError> {
        // Opus解码器使用目标采样率
        let decoder_config = OpusDecoderConfig::new(config.target_sample_rate, 1);
        let decoder = OpusDecoder::with_config(decoder_config).map_err(|e| InputProcessorError::DecoderInit(e.to_string()))?;

        Ok(Self {
            decoder: std::sync::Mutex::new(decoder),
            input_sample_rate: config.input_sample_rate,
            target_sample_rate: config.target_sample_rate,
            enable_resampling: config.enable_resampling,
        })
    }
}

impl AudioProcessor for OpusProcessor {
    fn process_audio_chunk(&mut self, data: &[u8]) -> Result<Vec<f32>, InputProcessorError> {
        // 直接解码完整的Opus帧，不需要缓冲区
        let mut decoder = self.decoder.lock().unwrap();
        match decoder.decode_frame(data) {
            Ok(decoded_frame) => {
                // 如果需要重采样且采样率不同
                if self.enable_resampling && self.input_sample_rate != self.target_sample_rate {
                    resample_audio(&decoded_frame, self.input_sample_rate, self.target_sample_rate)
                } else {
                    Ok(decoded_frame)
                }
            },
            Err(e) => {
                warn!("Opus解码失败: {}, 数据大小: {}字节", e, data.len());
                // 解码失败时返回空数据，让上层知道有数据被丢弃
                Ok(Vec::new())
            },
        }
    }

    fn reset(&mut self) {
        let mut decoder = self.decoder.lock().unwrap();
        decoder.reset();
    }
}

/// 音频输入处理器
pub struct AudioInputProcessor {
    config: AudioInputConfig,
    processor: Box<dyn AudioProcessor>,
}

impl AudioInputProcessor {
    /// 创建新的音频输入处理器
    pub fn new(config: AudioInputConfig) -> Result<Self, InputProcessorError> {
        let internal_config = config.to_internal_config();

        let processor: Box<dyn AudioProcessor> = if config.format.is_pcm() {
            Box::new(PcmProcessor::new(&internal_config))
        } else if config.format.is_opus() {
            Box::new(OpusProcessor::new(internal_config)?)
        } else {
            return Err(InputProcessorError::UnsupportedFormat(format!(
                "不支持的音频格式: {:?}",
                config.format
            )));
        };

        Ok(Self { config, processor })
    }

    /// 处理音频数据块 - 现在支持自动重采样！
    pub fn process_audio_chunk(&mut self, data: &[u8]) -> Result<Vec<f32>, InputProcessorError> {
        let samples = self.processor.process_audio_chunk(data)?;
        Ok(samples)
    }

    /// 获取配置信息
    pub fn get_config(&self) -> &AudioInputConfig {
        &self.config
    }

    /// 更新配置
    pub fn update_config(&mut self, new_config: AudioInputConfig) -> Result<(), InputProcessorError> {
        // 如果格式或采样率发生变化，需要重新创建处理器
        if new_config.format != self.config.format || new_config.sample_rate != self.config.sample_rate {
            self.config = new_config.clone();
            let internal_config = new_config.to_internal_config();

            self.processor = if new_config.format.is_pcm() {
                Box::new(PcmProcessor::new(&internal_config))
            } else if new_config.format.is_opus() {
                Box::new(OpusProcessor::new(internal_config)?)
            } else {
                return Err(InputProcessorError::UnsupportedFormat(format!(
                    "不支持的音频格式: {:?}",
                    new_config.format
                )));
            };
        } else {
            self.config = new_config;
        }

        Ok(())
    }

    /// 重置处理器状态
    pub fn reset(&mut self) {
        self.processor.reset();
    }
}

/// 音频输入错误类型
#[derive(Debug, thiserror::Error)]
pub enum InputProcessorError {
    #[error("解码器初始化失败: {0}")]
    DecoderInit(String),

    #[error("解码器未初始化")]
    DecoderNotInitialized,

    #[error("音频转换错误: {0}")]
    Convert(String),

    #[error("Opus解码错误: {0}")]
    OpusDecode(#[from] OpusError),

    #[error("不支持的音频格式: {0}")]
    UnsupportedFormat(String),

    #[error("其他错误: {0}")]
    Other(String),
}
