//! 文本-LLM-TTS Pipeline
//!
//! 复用 ModularPipeline 的 LLM 和 TTS 任务，仅替换 ASR 为文本输入

pub mod orchestrator;
mod text_input_task;

pub use orchestrator::LlmTtsPipeline;
