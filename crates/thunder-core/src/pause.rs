use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Cooperative pause primitive checked at tool-call and turn boundaries.
#[derive(Debug, Default)]
pub struct PauseGate {
    paused: AtomicBool,
    notify: Notify,
}

impl PauseGate {
    pub fn new() -> Self {
        Self {
            paused: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub async fn wait_if_paused(&self) {
        while self.is_paused() {
            self.notify.notified().await;
        }
    }
}
