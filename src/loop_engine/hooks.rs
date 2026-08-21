use crate::types::event::AgentEvent;
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct AgentEventDispatcher {
    sender: broadcast::Sender<AgentEvent>,
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

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.sender.subscribe()
    }

    pub fn emit(&self, event: AgentEvent) {
        // Silently ignore if no active subscribers
        let _ = self.sender.send(event);
    }
}
