//! State management: language, voice, engine detection and tracking.

use anyhow::Result;
use tracing::info;

use crate::audio::OutputAudioConfig;
use crate::tts::minimax::VoiceSetting;

use super::TtsController;

impl TtsController {
    /// Set language (from start session's asr_language).
    pub async fn set_language(&self, language: Option<String>) {
        let mut guard = self.language.lock().await;
        *guard = language;
    }

    /// Set language pair (for interpretation mode TTS language selection).
    pub async fn set_language_pair(&self, from_lang: String, to_lang: String) {
        let mut guard = self.language_pair.lock().await;
        *guard = Some((from_lang, to_lang));
    }

    /// Set stop-pending flag.
    pub fn set_stop_pending(&self, value: bool) {
        self.stop_pending.store(value, std::sync::atomic::Ordering::Release);
    }

    /// Read stop-pending flag.
    pub fn is_stop_pending(&self) -> bool {
        self.stop_pending.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Update voice settings (speaker, speed, volume, pitch, emotion).
    pub async fn update_voice_setting(&self, new_voice_setting: VoiceSetting) -> Result<()> {
        info!(
            "🔄 更新TTS语音设置: voice_id={:?}, speed={:?}, vol={:?}, pitch={:?}, emotion={:?}",
            new_voice_setting.voice_id, new_voice_setting.speed, new_voice_setting.vol, new_voice_setting.pitch, new_voice_setting.emotion
        );

        {
            let mut voice_setting_guard = self.voice_setting.lock().await;
            *voice_setting_guard = Some(new_voice_setting.clone());
            info!("✅ 内部语音设置已更新");
        }

        {
            let pool_client_guard = self.pool_client.lock().await;
            if let Some(client_arc) = pool_client_guard.as_ref() {
                let _ = {
                    let mut client = client_arc.lock().await;
                    if let Some(current_voice_setting) = (*client).get_voice_setting_mut() {
                        *current_voice_setting = Some(new_voice_setting.clone());
                        info!("✅ TTS客户端语音设置已更新 (MiniMax下在下次start_task生效)");
                    }
                    false
                };
            }
        }

        info!("✅ TTS语音设置更新完成");
        Ok(())
    }

    /// Get current voice ID (from setting or default config).
    pub async fn current_voice_id(&self) -> Option<String> {
        let voice_from_setting = {
            let guard = self.voice_setting.lock().await;
            guard.as_ref().and_then(|vs| vs.voice_id.clone()).filter(|id| !id.is_empty())
        };

        if voice_from_setting.is_some() {
            return voice_from_setting;
        }

        self.tts_config.as_ref().and_then(|cfg| cfg.default_voice_id.clone())
    }

    /// Configure full audio output config.
    pub async fn configure_output_config(&self, config: OutputAudioConfig) -> Result<()> {
        info!("🔄 TtsController配置音频输出配置: {:?}", config);

        let mut session_sender = self.session_audio_sender.lock().await;
        session_sender.configure_output_config(config).await?;

        info!("✅ TtsController音频输出配置已更新");
        Ok(())
    }
}
