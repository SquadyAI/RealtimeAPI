//! TTS-Only Pipeline - 独立的文本转语音管线
//!
//! 支持直接接收文本输入，输出TTS音频流
//! 不依赖ASR和LLM组件

pub mod config;
pub mod enhanced_streaming_pipeline;

// 🔧 标准TTS配置导出
pub use config::{TtsInputTimeout, TtsProcessorConfig};
pub use enhanced_streaming_pipeline::{
    EnhancedStreamingTtsOnlyPipeline, TtsConnectionStatus, create_enhanced_tts_only_pipeline, create_enhanced_tts_only_pipeline_with_audio_format, create_enhanced_tts_only_pipeline_with_opus,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioFormat, OpusEncoderConfig, OutputAudioConfig};

    #[test]
    fn test_opus_output_config_creation() {
        // 测试创建 Opus 输出配置
        let opus_config = OpusEncoderConfig { bitrate: 64000, frame_duration_ms: Some(20), complexity: 5, ..Default::default() };

        let output_config = OutputAudioConfig::opus(20, opus_config);

        assert_eq!(output_config.format, AudioFormat::Opus);
        assert_eq!(output_config.slice_ms, 20);
        assert_eq!(output_config.opus_config.as_ref().unwrap().bitrate, 64000);
        assert_eq!(output_config.opus_config.as_ref().unwrap().frame_duration_ms, Some(20));
        assert_eq!(output_config.opus_config.as_ref().unwrap().complexity, 5);
    }

    #[test]
    fn test_tts_processor_config_with_opus() {
        // 测试 TtsProcessorConfig 包含 Opus 配置
        let opus_config = OpusEncoderConfig::default();
        let output_config = OutputAudioConfig::opus(20, opus_config);

        let processor_config = TtsProcessorConfig { output_audio_config: Some(output_config.clone()), ..Default::default() };

        assert!(processor_config.output_audio_config.is_some());
        assert_eq!(processor_config.output_audio_config.unwrap().format, AudioFormat::Opus);
    }

    #[test]
    fn test_opus_config_validation() {
        // 测试 Opus 配置验证
        let opus_config = OpusEncoderConfig { frame_duration_ms: Some(15), ..Default::default() }; // 非标准帧长

        let mut output_config = OutputAudioConfig::opus(15, opus_config);

        // 验证配置
        assert!(output_config.validate().is_err());

        // 自动纠正
        output_config.auto_correct();
        assert!(output_config.validate().is_ok());
        assert_eq!(output_config.slice_ms, 10); // 15ms 最接近 10ms
    }
}
