use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

#[derive(Debug)]
struct TrackedTask {
    name: String,
    handle: JoinHandle<()>,
}

#[derive(Clone)]
pub struct SessionTaskScope {
    session_id: String,
    shutdown_tx: watch::Sender<bool>,
    tasks: Arc<Mutex<Vec<TrackedTask>>>,
}

impl SessionTaskScope {
    pub fn new(session_id: impl Into<String>) -> Self {
        let (shutdown_tx, _shutdown_rx) = watch::channel(false);
        Self {
            session_id: session_id.into(),
            shutdown_tx,
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    pub async fn spawn<F>(&self, name: impl Into<String>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.track(name.into(), tokio::spawn(future)).await;
    }

    pub async fn track(&self, name: String, handle: JoinHandle<()>) {
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|task| !task.handle.is_finished());
        tasks.push(TrackedTask { name, handle });
    }

    pub async fn shutdown(&self, grace_period: Duration) {
        info!("🔹 SessionTaskScope shutdown start: session={}", self.session_id);
        let _ = self.shutdown_tx.send(true);

        if !grace_period.is_zero() {
            tokio::time::sleep(grace_period).await;
        }

        let mut tasks = self.tasks.lock().await;
        for task in tasks.iter_mut() {
            if !task.handle.is_finished() {
                debug!("🚫 abort task: session={}, task={}", self.session_id, task.name);
                task.handle.abort();
            }
        }

        let drained = std::mem::take(&mut *tasks);
        drop(tasks);

        for task in drained {
            match task.handle.await {
                Ok(()) => debug!("✅ task exited: session={}, task={}", self.session_id, task.name),
                Err(err) if err.is_cancelled() => debug!("🔹 task cancelled: session={}, task={}", self.session_id, task.name),
                Err(err) => warn!(
                    "⚠️ task join failed: session={}, task={}, error={}",
                    self.session_id, task.name, err
                ),
            }
        }

        info!("✅ SessionTaskScope shutdown complete: session={}", self.session_id);
    }

    pub fn is_shutdown(&self) -> bool {
        *self.shutdown_tx.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::SessionTaskScope;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn shutdown_signals_and_joins_tracked_tasks() {
        let scope = SessionTaskScope::new("test-session");
        let finished = Arc::new(AtomicBool::new(false));

        let finished_task = finished.clone();
        let mut shutdown_rx = scope.subscribe_shutdown();
        scope
            .spawn("wait-for-shutdown", async move {
                loop {
                    if *shutdown_rx.borrow() {
                        finished_task.store(true, Ordering::Release);
                        break;
                    }

                    if shutdown_rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;

        scope.shutdown(Duration::from_millis(10)).await;

        assert!(scope.is_shutdown());
        assert!(finished.load(Ordering::Acquire));
    }
}
