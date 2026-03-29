use crate::audio::OutputAudioConfig;
use std::time::Duration;

/// TTS输入超时配置
#[derive(Debug, Clone)]
pub struct TtsInputTimeout {
    /// 超时时长
    pub timeout_duration: Duration,
    /// 是否启用超时警告
    pub enable_warning: bool,
    /// 警告间隔
    pub warning_interval: Duration,
}

impl Default for TtsInputTimeout {
    fn default() -> Self {
        Self {
            timeout_duration: Duration::from_secs(300),
            enable_warning: true,
            warning_interval: Duration::from_secs(60),
        }
    }
}

/// 标准TTS文本处理器配置（原EnhancedTextProcessorConfig）
#[derive(Debug, Clone)]
pub struct TtsProcessorConfig {
    /// 最大队列大小
    pub max_queue_size: usize,
    /// 处理间隔(ms)
    pub processing_interval_ms: u64,
    /// 音频分片目标时间长度(毫秒)，支持PCM和Opus统一时间分片
    pub chunk_size_target: usize,
    /// 发送速率倍数
    pub send_rate_multiplier: f64,
    /// 初始爆发发送块数
    pub initial_burst_count: usize,
    /// 初始爆发延迟(ms)
    pub initial_burst_delay_ms: u64,
    /// 输入超时配置
    pub input_timeout: Option<TtsInputTimeout>,
    /// 🆕 音频输出配置（支持PCM和Opus格式）
    pub output_audio_config: Option<OutputAudioConfig>,
}

impl Default for TtsProcessorConfig {
    fn default() -> Self {
        Self {
            max_queue_size: 100,
            processing_interval_ms: 50,
            chunk_size_target: 20,
            send_rate_multiplier: 1.01,
            initial_burst_count: 0,
            initial_burst_delay_ms: 5,
            input_timeout: Some(TtsInputTimeout::default()),
            output_audio_config: Some(OutputAudioConfig::default_pcm(20)),
        }
    }
}
