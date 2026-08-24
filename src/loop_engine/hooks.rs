use crate::types::event::ObservedEvent;
use tokio::sync::broadcast;

/// Lossy sidecar for fire-and-forget subscribers (`subscribe_events`).
/// The reliable per-run stream lives on [`crate::AgentHandle::events`].
#[derive(Clone)]
pub struct AgentEventDispatcher {
    sender: broadcast::Sender<ObservedEvent>,
}

impl Default for AgentEventDispatcher {
    fn default() -> Self {
        Self::new(128)
    }
}

impl AgentEventDispatcher {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ObservedEvent> {
        self.sender.subscribe()
    }

    pub fn emit(&self, event: ObservedEvent) {
        // Silently ignore if no active subscribers
        let _ = self.sender.send(event);
    }
}
