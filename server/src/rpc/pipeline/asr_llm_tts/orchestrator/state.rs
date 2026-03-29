//! State accessors, resource helpers, and audio blocking utilities for ModularPipeline.

use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

use super::super::audio_blocking_service::{AudioBlockingActivator, AudioBlockingChecker};
use super::super::simple_interrupt_manager::SimpleInterruptManager;
use super::super::types::SharedFlags;
use super::{ConfigUpdateEvent, ConnectionStatus, ModularPipeline};

impl ModularPipeline {
    // ============================
    // State accessors
    // ============================

    /// Get shared flags reference.
    pub fn get_shared_flags(&self) -> &Arc<SharedFlags> {
        &self.shared_flags
    }

    /// Get simplified interrupt manager reference.
    pub fn get_simple_interrupt_manager(&self) -> &Arc<SimpleInterruptManager> {
        &self.simple_interrupt_manager
    }

    /// Get config update sender reference.
    pub fn get_config_update_tx(&self) -> &Arc<Mutex<Option<mpsc::UnboundedSender<ConfigUpdateEvent>>>> {
        &self.config_update_tx
    }

    /// Get pacing params (burst_count, burst_delay_ms, send_rate_multiplier).
    pub fn get_pacing_params(&self) -> (usize, u64, f64) {
        (self.initial_burst_count, self.initial_burst_delay_ms, self.send_rate_multiplier)
    }

    /// Get current audio output config.
    pub async fn get_audio_output_config(&self) -> crate::audio::OutputAudioConfig {
        let guard = self.audio_output_config.lock().await;
        guard.clone()
    }

    /// Get text_done_signal_only flag (shared reference).
    pub fn get_text_done_signal_only_flag(&self) -> Arc<AtomicBool> {
        self.text_done_signal_only.clone()
    }

    /// Get signal_only flag (shared reference).
    pub fn get_signal_only_flag(&self) -> Arc<AtomicBool> {
        self.signal_only.clone()
    }

    /// Get TTS config (for VisionTts inheritance).
    pub fn get_tts_config(&self) -> Option<MiniMaxConfig> {
        self.tts_config.clone()
    }

    /// Get voice setting (for VisionTts inheritance).
    pub fn get_voice_setting(&self) -> Option<VoiceSetting> {
        self.voice_setting.clone()
    }

    /// Get idle timer reset stats.
    pub fn get_idle_timer_stats(&self) -> (u64, std::time::Duration) {
        let last_reset_secs = self.last_idle_reset_time.load(Ordering::Acquire);
        let last_reset_duration = std::time::Duration::from_secs(last_reset_secs);
        (last_reset_secs, last_reset_duration)
    }

    // ============================
    // TTS session & preconnection
    // ============================

    /// Check TTS preconnection status.
    pub async fn get_tts_preconnection_status(&self) -> Option<Result<(), String>> {
        let result_guard = self.tts_preconnection_result.lock().await;
        result_guard.clone()
    }

    /// Wait for TTS preconnection (with timeout).
    pub async fn wait_for_tts_preconnection(&self, timeout_ms: u64) -> Result<(), String> {
        let timeout = std::time::Duration::from_millis(timeout_ms);
        let start_time = std::time::Instant::now();

        loop {
            {
                let result_guard = self.tts_preconnection_result.lock().await;
                if let Some(result) = result_guard.as_ref() {
                    return result.clone();
                }
            }

            if start_time.elapsed() >= timeout {
                return Err(format!("TTS预连接等待超时: {}ms", timeout_ms));
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// Get connection status overview.
    pub async fn get_connection_status(&self) -> ConnectionStatus {
        let tts_preconnected = self.get_tts_preconnection_status().await;

        ConnectionStatus {
            tts_preconnected: tts_preconnected.as_ref().is_some_and(|r| r.is_ok()),
            session_id: self.session_id.clone(),
        }
    }

    /// Release TTS session (volcEngine-tts client based).
    pub(crate) async fn release_tts_session(&self) -> Result<()> {
        if self.tts_session_created.load(Ordering::Acquire) {
            info!("🔓 释放VolcEngine TTS会话: {}", self.session_id);
            self.tts_session_created.store(false, Ordering::Release);
            info!("✅ TTS会话标志已重置: {}", self.session_id);
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Release TTS session (manual, public).
    pub async fn release_tts_session_manual(&self) -> Result<()> {
        self.release_tts_session().await
    }

    /// Check if TTS session exists.
    pub async fn has_tts_session(&self) -> bool {
        self.tts_session_created.load(Ordering::Acquire)
    }

    // ============================
    // Audio blocking
    // ============================

    /// Activate audio blocking lock (called when ASR output → LLM).
    pub async fn activate_audio_blocking_lock(&self) {
        self.audio_blocking_service.activate_lock("pipeline").await;
    }

    /// Check if audio should be blocked.
    pub async fn should_block_audio(&self) -> bool {
        self.audio_blocking_service.should_block_audio().await
    }

    /// Create an audio blocking checker (for worker tasks).
    pub fn create_audio_blocking_checker(&self) -> AudioBlockingChecker {
        self.audio_blocking_service.checker()
    }

    /// Create an audio blocking activator (for parallel processing tasks).
    pub fn create_audio_blocking_activator(&self) -> AudioBlockingActivator {
        self.audio_blocking_service.activator()
    }

    // ============================
    // Audio channel health
    // ============================

    /// Check if audio input channels and ASR task are healthy.
    pub async fn check_audio_channels_healthy(&self) -> Result<bool> {
        let input_tx_available = self.input_tx_slot.is_set();
        let asr_task_running = self.asr_task_running.load(Ordering::Acquire);

        let is_healthy = input_tx_available && asr_task_running;

        if is_healthy {
            info!("✅ 音频通道和ASR任务状态正常: session_id={}", self.session_id);
        } else {
            warn!(
                "⚠️ 输入通道或ASR任务状态异常: session_id={}, input_tx={}, asr_running={}",
                self.session_id, input_tx_available, asr_task_running
            );
        }

        Ok(is_healthy)
    }
}
