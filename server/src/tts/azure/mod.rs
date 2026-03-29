//! Azure TTS 模块
//!
//! 支持 140+ 语言，600+ 神经网络声音
//! 通过 Azure 认知服务语音 API 进行流式语音合成

mod client;
mod config;
mod types;
mod voice_mapping;

pub use client::AzureTtsClient;
pub use config::AzureTtsConfig;
pub use types::AzureTtsError;
pub use voice_mapping::{AZURE_VOICE_MAP, get_voice_for_language, is_language_supported};
