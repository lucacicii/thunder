use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thunder_protocol::ChatMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueBehavior {
    Steer,
    FollowUp,
}

impl QueueBehavior {
    pub fn label(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueMode {
    All,
    #[default]
    OneAtATime,
}

impl QueueMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "all" => Some(Self::All),
            "one" | "one-at-a-time" | "one_at_a_time" | "single" => Some(Self::OneAtATime),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::OneAtATime => "one-at-a-time",
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    mode: QueueMode,
    messages: Vec<ChatMessage>,
}

#[derive(Debug, Default)]
pub struct PendingQueue {
    inner: Mutex<Inner>,
}

impl PendingQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn enqueue(&self, message: ChatMessage) {
        self.inner.lock().messages.push(message);
    }

    pub fn peek(&self) -> Vec<ChatMessage> {
        let inner = self.inner.lock();
        match inner.mode {
            QueueMode::All => inner.messages.clone(),
            QueueMode::OneAtATime => inner.messages.first().cloned().into_iter().collect(),
        }
    }

    pub fn drain(&self) -> Vec<ChatMessage> {
        let mut inner = self.inner.lock();
        let take = match inner.mode {
            QueueMode::All => inner.messages.len(),
            QueueMode::OneAtATime => usize::from(!inner.messages.is_empty()),
        };
        inner.messages.drain(..take).collect()
    }

    pub fn clear(&self) -> Vec<ChatMessage> {
        std::mem::take(&mut self.inner.lock().messages)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().messages.is_empty()
    }

    pub fn set_mode(&self, mode: QueueMode) {
        self.inner.lock().mode = mode;
    }

    pub fn mode(&self) -> QueueMode {
        self.inner.lock().mode
    }

    pub fn texts(&self) -> Vec<String> {
        self.inner
            .lock()
            .messages
            .iter()
            .filter_map(|m| m.content_str().map(str::to_string))
            .collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueSnapshot {
    pub steering: Vec<String>,
    pub follow_up: Vec<String>,
}

impl QueueSnapshot {
    pub fn total(&self) -> usize {
        self.steering.len() + self.follow_up.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steering.is_empty() && self.follow_up.is_empty()
    }
}

#[derive(Debug, Default)]
pub struct SteerQueues {
    pub steering: PendingQueue,
    pub follow_up: PendingQueue,
}

impl SteerQueues {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new())
    }

    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            steering: self.steering.texts(),
            follow_up: self.follow_up.texts(),
        }
    }

    pub fn clear_all(&self) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
        (self.steering.clear(), self.follow_up.clear())
    }
}
