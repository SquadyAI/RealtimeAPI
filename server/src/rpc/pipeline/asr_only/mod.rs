//! ASR-only 管线模块
//!
//! 仅提供语音识别功能，不包含 LLM 和 TTS 处理。
//! 适用于只需要语音转文字的场景。

mod orchestrator;

pub use orchestrator::AsrOnlyPipeline;
