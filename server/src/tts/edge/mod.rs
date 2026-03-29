//! Edge TTS 模块
//!
//! 基于微软 Edge 浏览器内置的免费 TTS 服务
//! 支持 100+ 语言，覆盖 lan.xlsx 中 97.1% 的语言

mod client;
mod config;
pub mod mp3_decoder;
mod types;
mod voice_mapping;

pub use client::EdgeTtsClient;
pub use config::EdgeTtsConfig;
pub use mp3_decoder::resample_to_16k;
pub use types::EdgeTtsError;
pub use voice_mapping::{EDGE_TTS_VOICE_MAP, get_voice_for_language};
