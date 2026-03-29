//! MiniMax TTS 模块
//!
//! 提供MiniMax语音合成服务支持，包括：
//! - HTTP 流式语音合成
//! - 音色快速复刻
//! - 流式音频输出

pub mod config;
pub mod http_client;
pub mod lang;
pub mod types;
pub mod voice_library;

pub use config::MiniMaxConfig;
pub use http_client::{MiniMaxHttpOptions, MiniMaxHttpTtsClient};
pub use lang::normalize_minimax_lang;
pub use types::{AudioChunk, AudioSetting, MiniMaxError, PronunciationDict, TimbreWeight, VoiceSetting};
pub use voice_library::{VoiceLibrary, VoiceLibraryConfig, global_voice_library};
