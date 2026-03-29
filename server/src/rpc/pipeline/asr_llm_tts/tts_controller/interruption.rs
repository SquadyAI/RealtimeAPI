//! Interruption: interrupt session, abort finish wait, reuse checks.

use anyhow::Result;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use tracing::info;

use super::TtsController;

impl TtsController {
    /// Set/replace the abort sender for background SessionFinished wait.
    pub async fn set_finish_wait_abort_tx(&self, tx: broadcast::Sender<()>) {
        let mut guard = self.finish_wait_abort_tx.lock().await;
        *guard = Some(tx);
        info!("🔗 已设置finish_wait_abort_tx (可用于中断后台等待)");
    }

    /// Trigger abort of background SessionFinished wait.
    pub async fn abort_finish_wait(&self) {
        let mut guard = self.finish_wait_abort_tx.lock().await;
        if let Some(tx) = guard.take() {
            let _ = tx.send(());
            info!("🛑 已发送finish_wait中断信号");
        } else {
            info!("ℹ️ 无finish_wait_abort_tx可中断，忽略");
        }
    }

    /// Interrupt the active TTS stream and prepare a new client.
    pub async fn interrupt_session(&self) -> Result<()> {
        // 1) Abort current stream
        let existing_client = {
            let guard = self.pool_client.lock().await;
            guard.clone()
        };

        if let Some(client_arc) = existing_client {
            let mut client = client_arc.lock().await;
            client.abort_active_stream();
            info!("🛑 已请求中断当前TTS流: client_id={}", client.get_client_id());
        } else {
            info!("ℹ️ 无当前TTS客户端可打断");
        }

        // 2) Stop local audio pipeline (clear buffer, switch generation)
        {
            let session_sender = self.session_audio_sender.lock().await;
            let _ = session_sender.force_clear_buffer().await;
            drop(session_sender);
            let new_gen = self.audio_subscription_gen.fetch_add(1, Ordering::AcqRel) + 1;
            if let Some(tx) = self.audio_gen_watch_tx.lock().await.as_ref() {
                let _ = tx.send(new_gen);
            }
        }

        // 3) Create new client for next text
        self.get_or_create_client().await?;

        Ok(())
    }

    /// Check connection health.
    pub async fn is_connection_healthy(&self) -> bool {
        let pool_client_guard = self.pool_client.lock().await;
        if let Some(client_arc) = pool_client_guard.as_ref() {
            let client = client_arc.lock().await;
            client.is_connected()
        } else {
            false
        }
    }

    /// Smart reuse check: can we reuse or quickly interrupt to get a usable client?
    pub async fn can_reuse_or_interrupt_client(&self) -> bool {
        if let Ok(pool_client_guard) = self.pool_client.try_lock()
            && let Some(client_arc) = pool_client_guard.as_ref()
        {
            if let Ok(client) = client_arc.try_lock() {
                return client.is_available() || client.is_connected();
            } else {
                return false;
            }
        }
        false
    }

    /// Check if preheat session is available.
    pub async fn has_preheat_session_available(&self) -> bool {
        self.is_connection_healthy().await
    }
}
