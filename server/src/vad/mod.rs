// Silero Voice Activity Detection (VAD) - Rust Implementation with Zero-Copy Optimizations
//
// 模块结构：
// - iterator: VADIterator (SileroVAD + 可选SmartTurn二层过滤)
// - semantic_vad: SmartTurn模型和特征提取
// - model: SileroVAD ONNX模型
// - engine: VAD引擎，管理Session创建
// - config: VAD配置

pub mod config;
pub mod engine;
pub mod iterator;
/// ONNX 模型文件内容
pub mod model;
/// 语义VAD模块 (SmartTurn)
pub mod semantic_vad;

pub use config::{VADConfig, VADPoolConfig};
pub use engine::VADEngine;
pub use iterator::{VADIterator, VadEvent, VadState, run_timeout_monitor};
pub use model::SileroVAD;
// 重新导出语义VAD组件（可选使用）
pub use semantic_vad::{SmartTurnPredictor, SmartTurnSession, SmartTurnSessionPool, features};

pub static MODEL_DATA: &[u8] = include_bytes!("silero_vad_16k_op15.onnx");
pub static SMART_TURN_MODEL_DATA: &[u8] = include_bytes!("smart-turn-v3.1-raw.onnx");

/// Error types for the Silero VAD library
#[derive(Debug, thiserror::Error)]
pub enum VADError {
    /// Error occurred while loading the model
    #[error("Model loading error: {0}")]
    ModelLoad(String),
    /// Model initialization error
    #[error("Model initialization error: {0}")]
    ModelInitializationError(String),
    /// Invalid input parameters or data
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    /// Error during audio processing
    #[error("Audio processing error: {0}")]
    AudioProcessing(String),
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// ONNX Runtime error
    #[error("ONNX Runtime error: {0}")]
    Ort(#[from] ort::Error),
    /// Resource unavailable
    #[error("Resource unavailable: {0}")]
    ResourceUnavailable(String),
    /// Session error
    #[error("Session error: {0}")]
    SessionError(String),
    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigurationError(String),
}

/// Result type for the Silero VAD library
pub type VADResult<T> = std::result::Result<T, VADError>;
