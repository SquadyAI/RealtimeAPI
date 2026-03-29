//! TTS client controller (pipeline-level shared) — pooled mode.
//!
//! Split from `tts_task.rs` for clarity.  The struct definition lives here;
//! method groups are in sub-modules that each add an `impl TtsController` block.

mod interruption;
mod lifecycle;
mod state;

use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

use crate::rpc::pipeline::asr_llm_tts::session_audio_sender::SessionAudioSender;
use crate::rpc::tts_pool::{TtsClient, TtsEngineKind};
use crate::tts::minimax::{MiniMaxConfig, VoiceSetting};

/// TTS client controller (pipeline-level shared) — pooled mode.
pub struct TtsController {
    /// TTS pool client
    pub pool_client: Arc<Mutex<Option<Arc<Mutex<TtsClient>>>>>,
    /// Init mutex: prevent concurrent WS connection creation
    pub init_lock: Arc<tokio::sync::Mutex<()>>,
    /// Voice setting — wrapped with Arc<Mutex> for runtime updates
    pub voice_setting: Arc<Mutex<Option<VoiceSetting>>>,
    /// Session-level audio sender manager
    pub session_audio_sender: Arc<Mutex<SessionAudioSender>>,
    /// Audio subscription generation: incremented on client return/switch
    pub audio_subscription_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Generation change notification channel (watch), replaces 250ms polling
    pub audio_gen_watch_tx: Arc<tokio::sync::Mutex<Option<tokio::sync::watch::Sender<u64>>>>,
    /// Interrupt manager for interruptible health checks
    pub interrupt_manager: Option<Arc<crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::SimpleInterruptManager>>,
    /// Last cancel/reset timestamp for adaptive reconnect backoff
    pub last_reset_or_cancel_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// Stop-pending flag: blocks TTS prewarm when set
    pub stop_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Abort channel for background SessionFinished wait
    pub finish_wait_abort_tx: Arc<Mutex<Option<broadcast::Sender<()>>>>,
    /// Cleanup notification receiver for SessionFinished
    pub finish_session_cleanup_rx: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    /// TTS config
    pub tts_config: Option<MiniMaxConfig>,
    /// Language (from start session's asr_language)
    pub language: Arc<Mutex<Option<String>>>,
    /// Language pair (from_language, to_language) for interpretation
    pub language_pair: Arc<Mutex<Option<(String, String)>>>,
    /// Per-turn detected language (keeps consistency within a turn, incl. prefetch)
    pub turn_detected_language: Arc<Mutex<Option<String>>>,
    /// Per-turn detected voice ID
    pub turn_detected_voice_id: Arc<Mutex<Option<String>>>,
    /// Per-turn confirmed TTS engine route
    pub turn_confirmed_engine: Arc<Mutex<Option<TtsEngineKind>>>,
}

impl Drop for TtsController {
    fn drop(&mut self) {
        let pool_client = self.pool_client.clone();
        tokio::spawn(async move {
            if let Ok(mut guard) = tokio::time::timeout(std::time::Duration::from_millis(300), pool_client.lock()).await
                && let Some(client_arc) = guard.take()
            {
                let mut client = client_arc.lock().await;
                client.abort_active_stream();
                client.cleanup().await;
            }
        });
    }
}

impl TtsController {
    /// Create a new TTS controller (pooled mode).
    pub fn new(tts_config: Option<MiniMaxConfig>, voice_setting: Option<VoiceSetting>) -> Self {
        Self {
            pool_client: Arc::new(Mutex::new(None)),
            init_lock: Arc::new(tokio::sync::Mutex::new(())),
            tts_config,
            voice_setting: Arc::new(Mutex::new(voice_setting)),
            session_audio_sender: Arc::new(Mutex::new(SessionAudioSender::new())),
            audio_subscription_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            audio_gen_watch_tx: Arc::new(tokio::sync::Mutex::new(None)),
            interrupt_manager: None,
            last_reset_or_cancel_at: Arc::new(Mutex::new(None)),
            stop_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            finish_wait_abort_tx: Arc::new(Mutex::new(None)),
            finish_session_cleanup_rx: Arc::new(Mutex::new(None)),
            language: Arc::new(Mutex::new(None)),
            language_pair: Arc::new(Mutex::new(None)),
            turn_detected_language: Arc::new(Mutex::new(None)),
            turn_detected_voice_id: Arc::new(Mutex::new(None)),
            turn_confirmed_engine: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the interrupt manager.
    pub fn set_interrupt_manager(&mut self, interrupt_manager: Arc<crate::rpc::pipeline::asr_llm_tts::simple_interrupt_manager::SimpleInterruptManager>) {
        self.interrupt_manager = Some(interrupt_manager);
    }

    /// Update pacing parameters (burst count, delay, rate multiplier).
    pub async fn update_pacing(&self, burst_count: usize, burst_delay_ms: u64, rate_multiplier: f64) {
        let mut sender_guard = self.session_audio_sender.lock().await;
        sender_guard.update_pacing(burst_count, burst_delay_ms, rate_multiplier);
    }
}
