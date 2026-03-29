//! VAD Configuration Management
//!
//! This module provides configuration management for VAD pools,
//! supporting both programmatic configuration and file-based configuration.

use serde::{Deserialize, Serialize};

use crate::env_utils::{env_bool_or_default, env_or_default};
use crate::vad::{VADError, VADResult};

/// Complete VAD system configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VADConfig {
    /// VAD pool configuration
    pub pool: VADPoolConfig,
}

/// VAD Pool Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VADPoolConfig {
    /// Maximum number of concurrent VAD instances
    pub max_instances: usize,
    /// Initial pool size (pre-allocated instances)
    pub initial_pool_size: usize,
    /// VAD detection threshold
    pub threshold: f32,
    /// Minimum silence duration to end speech segment (ms)
    pub min_silence_duration_ms: u32,
    /// Minimum speech duration to start speech segment (ms)
    /// VAD需要连续检测到语音达到此时长才算真正开始语音段，用于减少误触发
    pub min_speech_duration_ms: u32,
    /// Speech padding duration in samples (at 16kHz: 5120 samples = 320ms)
    /// 语音段开始时的padding时长，提供更多上下文以改善识别效果
    pub speech_pad_samples: u32,
    /// 是否启用完整推理 (default: false)
    /// false: 完全依赖增量推理，最大化降低延迟
    /// true: 语音段结束时进行完整推理，保证准确性
    pub enable_final_inference: bool,

    // === 语义VAD配置 (SemanticVAD / SmartTurn) ===
    /// SmartTurn话轮结束判断阈值 (default: 0.5)
    /// 概率 >= 阈值时认为用户语义上结束了说话
    pub semantic_threshold: f32,
    /// SmartTurn Session Pool 大小 (default: 4)
    /// 多个独立 Session 可以并行推理，提高吞吐量
    /// 建议值：4-8，根据 CPU 核心数和并发需求调整
    pub smart_turn_pool_size: usize,
    /// SmartTurn否决后的超时时间（毫秒），超时后自动发送ASR结果 (default: 300)
    /// 当SmartTurn判断用户只是停顿而非结束说话时，会暂存ASR结果
    /// 如果在此超时时间内没有新的语音开始，则自动发送暂存的ASR结果
    pub smart_turn_veto_timeout_ms: u32,
}

impl Default for VADPoolConfig {
    fn default() -> Self {
        Self {
            max_instances: env_or_default("VAD_MAX_INSTANCES", 200),
            initial_pool_size: env_or_default("VAD_INITIAL_POOL_SIZE", 8),
            threshold: env_or_default("VAD_THRESHOLD", 0.55), // 🔧 降低阈值从0.5到0.3，提高检测敏感度
            min_silence_duration_ms: env_or_default("VAD_MIN_SILENCE_MS", 200),
            // 🔧 略微降低连续语音触发时长，减少起始漏检
            min_speech_duration_ms: env_or_default("VAD_MIN_SPEECH_MS", 80),
            // 🔧 增加padding大小：5120（≈320ms）提供更充分的首音上下文（可被环境变量覆盖）
            speech_pad_samples: env_or_default("VAD_SPEECH_PAD_SAMPLES", 8000),
            enable_final_inference: env_bool_or_default("VAD_ENABLE_FINAL_INFERENCE", false),
            // 语义VAD配置
            semantic_threshold: env_or_default("VAD_SEMANTIC_THRESHOLD", 0.5),
            smart_turn_pool_size: env_or_default("VAD_SMART_TURN_POOL_SIZE", 4),
            smart_turn_veto_timeout_ms: env_or_default("VAD_SMART_TURN_VETO_TIMEOUT_MS", 300),
        }
    }
}

impl VADConfig {
    /// Validate configuration parameters
    pub fn validate(&self) -> VADResult<()> {
        // Validate pool configuration
        if self.pool.max_instances == 0 {
            return Err(VADError::ConfigurationError("max_instances must be greater than 0".to_string()));
        }

        if self.pool.initial_pool_size > self.pool.max_instances {
            return Err(VADError::ConfigurationError(
                "initial_pool_size cannot exceed max_instances".to_string(),
            ));
        }

        if self.pool.threshold < 0.0 || self.pool.threshold > 1.0 {
            return Err(VADError::ConfigurationError(
                "threshold must be between 0.0 and 1.0".to_string(),
            ));
        }
        Ok(())
    }
}
