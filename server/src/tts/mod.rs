//! TTS模块
//!
//! 提供多种TTS引擎支持

pub mod azure;
pub mod baidu;
pub mod edge;
pub mod minimax;
pub mod volc_engine;

// 显式导出，避免模块间命名冲突
// Baidu TTS: HTTP 客户端 (推荐) + WebSocket 客户端 (兼容保留)
pub use baidu::{
    BaiduAudioFormat,
    BaiduHttpTtsClient,
    BaiduHttpTtsRequest, // HTTP REST API (推荐)
    BaiduTtsClient,
    BaiduTtsConfig,
    BaiduTtsError,
    BaiduTtsErrorCode,
    BaiduTtsRequest, // WebSocket (兼容保留)
    SystemStartPayload,
};
pub use minimax::{
    AudioChunk, AudioSetting, MiniMaxConfig, MiniMaxError, MiniMaxHttpOptions, MiniMaxHttpTtsClient, PronunciationDict, TimbreWeight, VoiceLibrary, VoiceLibraryConfig, VoiceSetting,
    global_voice_library, normalize_minimax_lang,
};
pub use volc_engine::{VolcEngineConfig, VolcEngineRequest, VolcEngineTtsClient};

// Edge TTS: 免费的微软 Edge 浏览器内置 TTS（100+ 语言）
pub use edge::{EDGE_TTS_VOICE_MAP, EdgeTtsClient, EdgeTtsConfig, EdgeTtsError, get_voice_for_language};

// Azure TTS: 微软 Azure 认知服务语音 API（140+ 语言，600+ 声音）
pub use azure::{AZURE_VOICE_MAP, AzureTtsClient, AzureTtsConfig, AzureTtsError, get_voice_for_language as get_azure_voice};
