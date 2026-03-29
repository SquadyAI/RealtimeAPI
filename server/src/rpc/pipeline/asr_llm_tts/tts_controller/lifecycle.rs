//! Lifecycle methods: client creation, prewarm, return, cleanup, subscription.

use anyhow::{Result, anyhow};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::rpc::tts_pool::{TtsClient, TtsEngineKind, launch_synthesis};
use crate::tts::minimax::AudioChunk;

use super::TtsController;

impl TtsController {
    /// Get or create the local TTS client (double-check locking).
    pub async fn get_or_create_client(&self) -> Result<()> {
        let total_start = std::time::Instant::now();
        let call_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        info!("🎯 [get_or_create_client] 调用开始, call_id: {}", call_id);

        if self.is_stop_pending() {
            info!(
                "⏭️ [get_or_create_client] 跳过获取TTS客户端：stop_pending=true, call_id: {}",
                call_id
            );
            return Err(anyhow!("TTS 停止待决，跳过预热/获取"));
        }

        info!("🔒 [get_or_create_client] 尝试获取锁, call_id: {}", call_id);
        let mut pool_client_guard = self.pool_client.lock().await;
        info!("✅ [get_or_create_client] 已获取锁, call_id: {}", call_id);

        if let Some(client_arc) = pool_client_guard.as_ref() {
            let client = client_arc.lock().await;
            if client.is_connected() {
                info!(
                    "✅ [get_or_create_client] TTS客户端已存在且已连接，无需重新获取, call_id: {}",
                    call_id
                );
                return Ok(());
            }
            info!("🔄 [get_or_create_client] 现有TTS客户端已断开，准备重建, call_id: {}", call_id);
        }

        *pool_client_guard = None;
        drop(pool_client_guard);
        info!("🔓 [get_or_create_client] 已释放锁，准备初始化, call_id: {}", call_id);

        let _init_guard = self.init_lock.lock().await;
        info!("🔒 [get_or_create_client] 获取初始化互斥锁, call_id: {}", call_id);

        {
            let pool_client_guard = self.pool_client.lock().await;
            if let Some(client_arc) = pool_client_guard.as_ref() {
                let client = client_arc.lock().await;
                if client.is_connected() {
                    info!(
                        "✅ [get_or_create_client] 初始化互斥期间发现已有已连接客户端，直接复用, call_id: {}",
                        call_id
                    );
                    return Ok(());
                }
            }
        }

        info!(
            "🔧 [get_or_create_client] 本地创建 HTTP TTS 客户端（不使用全局池）, call_id: {}",
            call_id
        );

        let config = self.tts_config.clone().unwrap_or_default();
        let voice_setting = { self.voice_setting.lock().await.clone() };

        let mut client = TtsClient::new(format!("minimax-local-{}", uuid::Uuid::new_v4()), config, voice_setting);

        let lang = { self.language.lock().await.clone() };
        client.set_language(lang);

        let lang_pair = { self.language_pair.lock().await.clone() };
        client.set_language_pair(lang_pair);

        info!("🔧 [get_or_create_client] 调用 TtsClient::initialize(), call_id: {}", call_id);
        match tokio::time::timeout(std::time::Duration::from_secs(6), client.initialize()).await {
            Ok(Ok(())) => {
                info!("✅ [get_or_create_client] TtsClient::initialize() 完成, call_id: {}", call_id);
            },
            Ok(Err(e)) => {
                warn!(
                    "⚠️ [get_or_create_client] TtsClient::initialize() 失败: {}, call_id: {}",
                    e, call_id
                );
                return Err(e);
            },
            Err(_) => {
                warn!(
                    "⚠️ [get_or_create_client] TtsClient::initialize() 超时(6s), call_id: {}",
                    call_id
                );
                return Err(anyhow!("TTS客户端初始化超时(6s)"));
            },
        }

        let client_arc = Arc::new(Mutex::new(client));
        info!("🔒 [get_or_create_client] 准备存入客户端，尝试获取锁, call_id: {}", call_id);
        let mut pool_client_guard = self.pool_client.lock().await;
        info!("✅ [get_or_create_client] 已获取锁准备存入, call_id: {}", call_id);

        if let Some(existing_arc) = pool_client_guard.as_ref() {
            let existing_client = existing_arc.lock().await;
            if existing_client.is_connected() {
                warn!(
                    "⚠️ [get_or_create_client] 检测到并发创建，使用已存在的客户端, call_id: {}",
                    call_id
                );
                return Ok(());
            }
        }

        *pool_client_guard = Some(client_arc);
        info!("💾 [get_or_create_client] 客户端已存入, call_id: {}", call_id);

        let elapsed = total_start.elapsed();
        info!(
            "✅ [get_or_create_client] 本地TTS客户端就绪，用时: {}ms, call_id: {}",
            elapsed.as_millis(),
            call_id
        );
        Ok(())
    }

    /// Prewarm the TTS client (uses the same path as normal acquisition).
    pub async fn prewarm(&self) -> Result<()> {
        if self.is_stop_pending() {
            info!("⏭️ 跳过TTS预热：stop_pending=true");
            return Err(anyhow!("TTS 停止待决，跳过预热"));
        }
        info!("🔧 预热：统一调用 get_or_create_client() 获取TTS客户端");
        self.get_or_create_client().await
    }

    /// Default MiniMax synthesis entry point.
    pub async fn synthesize_text(&self, text: &str) -> Result<()> {
        self.synthesize_with_engine(TtsEngineKind::MiniMax, text).await
    }

    /// Unified engine synthesis entry point (with timeout).
    pub async fn synthesize_with_engine(&self, engine: TtsEngineKind, text: &str) -> Result<()> {
        if self.is_stop_pending() {
            info!("🔄 收到合成请求，清除stop_pending标志");
            self.set_stop_pending(false);
        }

        let timeout_duration = if text == "__END_OF_TEXT__" || text == "__TURN_COMPLETE__" {
            std::time::Duration::from_secs(5)
        } else {
            std::time::Duration::from_secs(3)
        };

        match tokio::time::timeout(timeout_duration, self.synthesize_with_engine_internal(engine, text)).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    "⚠️ synthesize_with_engine({:?}) 超时({:?})，可能存在锁竞争或网络延迟",
                    engine, timeout_duration
                );
                Err(anyhow!("synthesize_with_engine 操作超时"))
            },
        }
    }

    /// Internal synthesis impl: prepare plan + launch streaming task.
    pub(crate) async fn synthesize_with_engine_internal(&self, engine: TtsEngineKind, text: &str) -> Result<()> {
        self.get_or_create_client().await?;

        let client_arc = {
            let pool_client_guard = self.pool_client.lock().await;
            pool_client_guard.as_ref().cloned().ok_or_else(|| anyhow!("TTS 客户端不可用"))?
        };

        let plan = {
            let mut turn_lang_guard = self.turn_detected_language.lock().await;
            let mut turn_voice_guard = self.turn_detected_voice_id.lock().await;
            let mut client = client_arc.lock().await;
            client.prepare_synthesis(engine, text, &mut turn_lang_guard, &mut turn_voice_guard)?
        };

        info!(
            "🎯 启动 {:?} 合成任务: client_id={}, text_preview='{}'",
            engine,
            plan.client_id,
            text.chars().take(30).collect::<String>()
        );

        launch_synthesis(client_arc, plan);
        Ok(())
    }

    /// Prepare for a new turn (clear per-turn caches).
    pub async fn prepare_new_turn_session(&self) -> Result<()> {
        info!("🔄 准备新轮次 (HTTP TTS，无显式 session 状态)");
        self.set_stop_pending(false);

        {
            let mut lang_guard = self.turn_detected_language.lock().await;
            let mut voice_guard = self.turn_detected_voice_id.lock().await;
            let mut engine_guard = self.turn_confirmed_engine.lock().await;
            if lang_guard.is_some() || voice_guard.is_some() || engine_guard.is_some() {
                info!(
                    "🌐 重置轮次缓存: previous_lang={:?}, previous_voice={:?}, previous_engine={:?}",
                    lang_guard, voice_guard, engine_guard
                );
            }
            *lang_guard = None;
            *voice_guard = None;
            *engine_guard = None;
        }

        self.get_or_create_client().await
    }

    /// Reset client (lazy: return current, re-acquire on next use).
    pub async fn reset_client(&self) {
        info!("🔄 懒加载: 归还当前 TTS 客户端（打断清理模式），延迟到下次使用时再获取");
        self.return_client_with_mode(true).await;

        {
            let mut ts = self.last_reset_or_cancel_at.lock().await;
            *ts = Some(std::time::Instant::now());
        }
    }

    /// Return client to pool (normal cleanup).
    pub async fn return_client(&self) {
        self.return_client_with_mode(false).await;
    }

    /// Internal: return client with optional interrupt-cleanup mode.
    pub(crate) async fn return_client_with_mode(&self, is_interrupt_cleanup: bool) {
        if is_interrupt_cleanup {
            let session_sender = self.session_audio_sender.lock().await;
            let _ = session_sender.force_clear_buffer().await;
            drop(session_sender);
            let new_gen = self.audio_subscription_gen.fetch_add(1, Ordering::AcqRel) + 1;
            if let Some(tx) = self.audio_gen_watch_tx.lock().await.as_ref() {
                let _ = tx.send(new_gen);
            }
            info!("🔁 打断清理完成（旧客户端将后台自行回收）");
            return;
        }

        {
            let mut guard = self.pool_client.lock().await;
            if let Some(_client_arc) = guard.take() {
                info!("🔚 本轮完成，已回收MiniMax客户端（下轮将重建）");
            } else {
                info!("ℹ️ 本轮完成，无MiniMax客户端可回收");
            }
        }
    }

    /// Subscribe to the audio broadcast stream.
    pub async fn subscribe_audio(&self) -> Result<tokio::sync::broadcast::Receiver<AudioChunk>> {
        let pool_client_guard = self.pool_client.lock().await;
        if let Some(client_arc) = pool_client_guard.as_ref() {
            let client = client_arc.lock().await;
            if let Some(audio_rx) = client.subscribe_audio() {
                info!("✅ 成功订阅TTS客户端音频流（广播模式）");
                Ok(audio_rx)
            } else {
                warn!("⚠️ TTS客户端音频广播不可用");
                Err(anyhow!("TTS客户端音频广播不可用"))
            }
        } else {
            warn!("⚠️ 尝试订阅音频时TTS客户端不可用");
            Err(anyhow!("TTS客户端不可用"))
        }
    }

    /// Check if audio subscription is available (non-consuming).
    pub async fn has_audio_subscription(&self) -> bool {
        let pool_client_guard = self.pool_client.lock().await;
        if let Some(client_arc) = pool_client_guard.as_ref() {
            let client = client_arc.lock().await;
            client.subscribe_audio().is_some()
        } else {
            false
        }
    }

    /// End current TTS task (send abort signal and clear stream).
    pub async fn finish_current_task(&self) -> Result<()> {
        let pool_client_guard = self.pool_client.lock().await;
        if let Some(client_arc) = pool_client_guard.as_ref() {
            let mut client = client_arc.lock().await;
            client.abort_active_stream();
            info!("🔚 已请求结束当前TTS流");
        }
        Ok(())
    }
}
