use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::rpc::pipeline::SessionTaskScope;

#[derive(Clone)]
pub struct AudioBlockingChecker {
    state: Arc<AudioBlockingState>,
}

impl AudioBlockingChecker {
    pub async fn should_block_audio(&self) -> bool {
        self.state.should_block_audio().await
    }
}

#[derive(Clone)]
pub struct AudioBlockingActivator {
    state: Arc<AudioBlockingState>,
}

impl AudioBlockingActivator {
    pub async fn activate_lock(&self, response_id: &str) {
        self.state.activate_lock(response_id).await;
    }
}

/// 音频阻止服务，当获得响应时可以触发音频阻止，可以阻止当前正在播放的音频
#[derive(Clone)]
pub struct AudioBlockingService {
    state: Arc<AudioBlockingState>,
}

impl AudioBlockingService {
    pub fn new(session_id: String, enabled: bool, duration_ms: u64, task_scope: Arc<SessionTaskScope>) -> Self {
        Self {
            state: Arc::new(AudioBlockingState {
                lock: Arc::new(AtomicBool::new(false)),
                generation: Arc::new(AtomicU64::new(0)),
                lock_start_time: Arc::new(Mutex::new(None)),
                session_id,
                enabled,
                duration_ms,
                task_scope,
            }),
        }
    }

    pub fn checker(&self) -> AudioBlockingChecker {
        AudioBlockingChecker { state: self.state.clone() }
    }

    pub fn activator(&self) -> AudioBlockingActivator {
        AudioBlockingActivator { state: self.state.clone() }
    }

    pub async fn activate_lock(&self, response_id: &str) {
        self.state.activate_lock(response_id).await;
    }

    pub async fn should_block_audio(&self) -> bool {
        self.state.should_block_audio().await
    }
}

struct AudioBlockingState {
    lock: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    lock_start_time: Arc<Mutex<Option<std::time::Instant>>>,
    session_id: String,
    enabled: bool,
    duration_ms: u64,
    task_scope: Arc<SessionTaskScope>,
}

impl AudioBlockingState {
    async fn activate_lock(&self, response_id: &str) {
        if !self.enabled {
            debug!(
                "🔊 音频阻止未启用，跳过此操作: session={}, response_id={}",
                self.session_id, response_id
            );
            return;
        }

        info!(
            "🔊 ASR返回结果后启用LLM{}ms音频阻止: session={}, response_id={}",
            self.duration_ms, self.session_id, response_id
        );

        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.lock.store(true, Ordering::Release);
        {
            let mut lock_time = self.lock_start_time.lock().await;
            *lock_time = Some(std::time::Instant::now());
        }

        let lock = self.lock.clone();
        let active_generation = self.generation.clone();
        let lock_start_time = self.lock_start_time.clone();
        let session_id = self.session_id.clone();
        let duration_ms = self.duration_ms;
        self.task_scope
            .spawn("audio_blocking_unlock", async move {
                tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                if lock.load(Ordering::Acquire) && active_generation.load(Ordering::Acquire) == generation {
                    lock.store(false, Ordering::Release);
                    let mut guard = lock_start_time.lock().await;
                    *guard = None;
                    info!("🔔 {}ms音频阻止解除: session={}", duration_ms, session_id);
                }
            })
            .await;
    }

    async fn should_block_audio(&self) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.lock.load(Ordering::Acquire) {
            return false;
        }

        let mut should_unlock = false;
        {
            let lock_time_guard = self.lock_start_time.lock().await;
            if let Some(start_time) = *lock_time_guard
                && start_time.elapsed() >= std::time::Duration::from_millis(self.duration_ms)
            {
                should_unlock = true;
            }
        }

        if should_unlock {
            self.lock.store(false, Ordering::Release);
            let mut lock_time = self.lock_start_time.lock().await;
            *lock_time = None;
            debug!("🔔 {}ms音频阻止解除(checker): session={}", self.duration_ms, self.session_id);
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::AudioBlockingService;
    use crate::rpc::pipeline::SessionTaskScope;

    #[tokio::test]
    async fn lock_releases_after_timeout() {
        let scope = Arc::new(SessionTaskScope::new("audio-blocking-test"));
        let service = AudioBlockingService::new("audio-blocking-test".to_string(), true, 10, scope.clone());

        service.activate_lock("resp-1").await;
        assert!(service.should_block_audio().await);

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!service.should_block_audio().await);

        scope.shutdown(Duration::from_millis(1)).await;
    }

    #[tokio::test]
    async fn older_unlock_task_does_not_release_newer_activation() {
        let scope = Arc::new(SessionTaskScope::new("audio-blocking-test"));
        let service = AudioBlockingService::new("audio-blocking-test".to_string(), true, 25, scope.clone());

        service.activate_lock("resp-1").await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        service.activate_lock("resp-2").await;

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(service.should_block_audio().await);

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!service.should_block_audio().await);

        scope.shutdown(Duration::from_millis(1)).await;
    }
}
