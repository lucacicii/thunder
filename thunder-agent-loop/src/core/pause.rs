//! Cooperative pause primitive.
//!
//! Sits alongside [`tokio_util::sync::CancellationToken`] as a *product-neutral*
//! control primitive: the loop knows how to hold and resume a turn, but nothing
//! about who asks for it or why. Cancellation is irreversible; pausing is not,
//! which is why it is a separate token rather than a flag on the former.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// A resumable gate. While paused, [`PauseGate::wait_if_paused`] blocks.
///
/// The gate is checked at tool-call boundaries, so pausing never interrupts an
/// in-flight tool — it takes effect before the next one starts.
#[derive(Debug)]
pub struct PauseGate {
    paused: AtomicBool,
    notify: Notify,
}

impl Default for PauseGate {
    fn default() -> Self {
        Self::new()
    }
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

    /// Enter the paused state. Idempotent.
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        // Wake any waiter so it re-reads the flag (no-op if already blocked on
        // the correct state).
        self.notify.notify_waiters();
    }

    /// Leave the paused state and release every waiter.
    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Block while paused. Returns immediately when not paused.
    pub async fn wait_if_paused(&self) {
        while self.is_paused() {
            self.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn passes_through_when_not_paused() {
        let gate = PauseGate::new();
        // Must not hang.
        tokio::time::timeout(Duration::from_millis(50), gate.wait_if_paused())
            .await
            .expect("unpaused gate returns immediately");
    }

    #[tokio::test]
    async fn blocks_while_paused_and_releases_on_resume() {
        let gate = Arc::new(PauseGate::new());
        gate.pause();

        let waiter = {
            let g = Arc::clone(&gate);
            tokio::spawn(async move { g.wait_if_paused().await })
        };

        // Still blocked.
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "waiter must be parked while paused");

        gate.resume();
        tokio::time::timeout(Duration::from_millis(200), waiter)
            .await
            .expect("resume releases the waiter")
            .unwrap();
    }

    #[tokio::test]
    async fn pause_is_idempotent() {
        let gate = PauseGate::new();
        gate.pause();
        gate.pause();
        assert!(gate.is_paused());
        gate.resume();
        assert!(!gate.is_paused());
    }
}
