//! 同声传译管线模块

pub mod language_router;
pub mod orchestrator;
pub mod translation_task;

pub use orchestrator::TranslationPipeline;
