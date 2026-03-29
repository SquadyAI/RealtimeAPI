//! 会话数据持久化集成模块
//!
//! 无状态的会话数据持久化系统，基于upsert机制，支持流式accumulate
//! 设计原则：
//! 1. 完全无状态，不依赖管线层面的状态管理
//! 2. 基于 session_id + response_id 作为唯一键进行upsert
//! 3. 支持流式accumulate，LLM和TTS可以分别更新同一条记录
//! 4. 全局唯一访问，需要时再取用

use anyhow::Result;
use bytes::Bytes;
use tracing::{debug, info};

use crate::storage::session_data::AudioMetadata;

/// 从PCM S16LE音频数据创建音频元数据
pub fn create_audio_metadata_from_pcm_s16le(audio_data_len: usize, sample_rate: u32, channels: u16) -> AudioMetadata {
    // 计算音频时长：字节数 / (采样率 * 声道数 * 每样本字节数)
    let total_samples = audio_data_len / (2 * channels as usize);
    let duration_ms = (total_samples as f64 * 1000.0 / sample_rate as f64) as u32;

    AudioMetadata {
        format: "pcm_s16le".to_string(),
        sample_rate,
        channels,
        duration_ms,
        size_bytes: audio_data_len,
    }
}

/// 全局无状态辅助函数：保存用户音频分片
///
/// 供ASR任务直接调用的全局函数，完全无状态
/// 仅在VAD检测到is_first=true时调用，确保每次更新只设置一次
pub async fn save_user_audio_globally(session_id: &str, response_id: &str, audio_chunks: Bytes, audio_metadata: AudioMetadata) -> Result<()> {
    use crate::storage::GlobalSessionStoreManager;

    let Some(store) = GlobalSessionStoreManager::get() else {
        debug!("🔕 全局会话数据存储不可用，跳过用户音频保存");
        return Ok(());
    };

    debug!(
        "🎤 保存用户音频分片: session_id={}, response_id={}, audio_size={}字节",
        session_id,
        response_id,
        audio_chunks.len()
    );

    // 使用新的专门方法保存ASR音频数据
    store
        .save_asr_audio_data(response_id, session_id, Some(audio_chunks), Some(audio_metadata))
        .await?;

    info!("✅ 用户音频保存成功: session_id={}, response_id={}", session_id, response_id);
    Ok(())
}

/// 全局无状态辅助函数：保存对话元数据
///
/// 供LLM任务直接调用的全局函数，完全无状态
/// 确保每次更新只设置一次，并在Pipeline级别持久化
pub async fn save_conversation_metadata_globally(session_id: &str, response_id: &str, llm_to_tts_text: &str, is_final: bool) -> Result<()> {
    use crate::storage::GlobalSessionStoreManager;

    let Some(store) = GlobalSessionStoreManager::get() else {
        debug!("🔕 全局会话数据存储不可用，跳过对话元数据保存");
        return Ok(());
    };

    debug!(
        "📝 保存对话元数据: session_id={}, response_id={}, text_len={}, is_final={}",
        session_id,
        response_id,
        llm_to_tts_text.len(),
        is_final
    );

    // 创建metadata
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("response_id".to_string(), serde_json::json!(response_id));
    metadata.insert("data_type".to_string(), serde_json::json!("conversation_metadata"));
    metadata.insert("is_final".to_string(), serde_json::json!(is_final));

    // 使用新的专门方法保存对话元数据
    store
        .save_conversation_metadata(response_id, session_id, llm_to_tts_text.to_string(), Some(metadata))
        .await?;

    info!(
        "✅ 对话元数据保存成功: session_id={}, response_id={}, is_final={}",
        session_id, response_id, is_final
    );
    Ok(())
}

/// 全局无状态辅助函数：保存TTS音频数据
///
/// 供TTS任务直接调用的全局函数，完全无状态
/// 确保每次更新只设置一次，并在Pipeline级别持久化
pub async fn save_tts_audio_globally(session_id: &str, response_id: &str, tts_output_audio: Bytes, audio_metadata: AudioMetadata) -> Result<()> {
    use crate::storage::GlobalSessionStoreManager;

    let Some(store) = GlobalSessionStoreManager::get() else {
        debug!("🔕 全局会话数据存储不可用，跳过TTS音频保存");
        return Ok(());
    };

    debug!(
        "🎵 保存TTS音频: session_id={}, response_id={}, audio_size={}字节",
        session_id,
        response_id,
        tts_output_audio.len()
    );

    // 使用新的专门方法保存TTS音频数据
    store
        .save_tts_audio_data(response_id, session_id, Some(tts_output_audio), Some(audio_metadata))
        .await?;

    info!("✅ TTS音频保存成功: session_id={}, response_id={}", session_id, response_id);
    Ok(())
}
