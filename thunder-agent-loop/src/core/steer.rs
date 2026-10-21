//! Cooperative message queues for steering and follow-up input.
//!
//! Sits beside [`PauseGate`](crate::core::pause::PauseGate) as another
//! product-neutral control primitive: the loop knows how to splice a queued
//! user message into a run, but nothing about who queued it or why.
//!
//! Two queues share one type because they differ only in *when* they drain:
//!
//! * **steering** — delivered after the current turn's tool calls, before the
//!   next model request. A steering message can keep alive a run that would
//!   otherwise have concluded, which is the whole point: the user changes the
//!   agent's mind mid-flight.
//! * **follow-up** — delivered only once the run has nothing else to do.
//!
//! Neither ever interrupts a tool: both are drained at turn boundaries, the
//! same place [`PauseGate`](crate::core::pause::PauseGate) is checked.

use crate::types::message::ChatMessage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Which queue a message came from. The two differ only in *when* they drain,
/// so a host reporting one to the user (or restoring it to an editor) has to
/// say which it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueBehavior {
    /// Delivered after the current turn's tool calls, before the next request.
    Steer,
    /// Delivered only once the run has nothing else to do.
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

/// How many queued messages a single drain delivers.
///
/// `OneAtATime` is the default, and the reason is not throttling for its own
/// sake: several instructions arriving in one turn read as one blob, and the
/// model tends to answer only the last of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QueueMode {
    /// Deliver every queued message at once.
    All,
    /// Deliver one message per turn, in order.
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

/// A FIFO of user messages waiting to enter a run.
///
/// Cheap to clone around: hosts hold an `Arc` and push, the loop drains.
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

    /// Append a message. Ours to order, not the caller's.
    pub fn enqueue(&self, message: ChatMessage) {
        self.inner.lock().messages.push(message);
    }

    /// The messages a single drain would deliver, without removing them.
    pub fn peek(&self) -> Vec<ChatMessage> {
        let inner = self.inner.lock();
        match inner.mode {
            QueueMode::All => inner.messages.clone(),
            QueueMode::OneAtATime => inner.messages.first().cloned().into_iter().collect(),
        }
    }

    /// Remove and return the messages for this turn.
    pub fn drain(&self) -> Vec<ChatMessage> {
        let mut inner = self.inner.lock();
        let take = match inner.mode {
            QueueMode::All => inner.messages.len(),
            QueueMode::OneAtATime => usize::from(!inner.messages.is_empty()),
        };
        inner.messages.drain(..take).collect()
    }

    /// Remove everything and return it, so a host can hand the text back to the
    /// editor when a run is aborted instead of silently dropping it.
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

    /// Message text, for display. Images are not represented (they have no text).
    pub fn texts(&self) -> Vec<String> {
        self.inner
            .lock()
            .messages
            .iter()
            .filter_map(|m| m.content_str().map(str::to_string))
            .collect()
    }
}

/// What a host renders for its pending-message indicator.
///
/// One snapshot of both queues, so a UI cannot show a half-updated pair.
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

/// The pair of queues a run owns.
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

    /// Snapshot both queues together.
    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            steering: self.steering.texts(),
            follow_up: self.follow_up.texts(),
        }
    }

    /// Empty both, returning them for restoration into an editor.
    pub fn clear_all(&self) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
        (self.steering.clear(), self.follow_up.clear())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> ChatMessage {
        ChatMessage::user(text)
    }

    #[test]
    fn one_at_a_time_delivers_one_per_drain() {
        let q = PendingQueue::new();
        assert_eq!(q.mode(), QueueMode::OneAtATime);
        q.enqueue(msg("first"));
        q.enqueue(msg("second"));

        assert_eq!(q.len(), 2);
        let first = q.drain();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].content_str(), Some("first"));
        assert_eq!(q.len(), 1, "the rest stays queued");

        let second = q.drain();
        assert_eq!(second[0].content_str(), Some("second"));
        assert!(q.is_empty());
        assert!(q.drain().is_empty());
    }

    #[test]
    fn all_mode_delivers_the_whole_queue() {
        let q = PendingQueue::new();
        q.set_mode(QueueMode::All);
        q.enqueue(msg("a"));
        q.enqueue(msg("b"));

        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert!(q.is_empty());
    }

    #[test]
    fn peek_does_not_consume() {
        let q = PendingQueue::new();
        q.enqueue(msg("only"));
        assert_eq!(q.peek().len(), 1);
        assert_eq!(q.peek().len(), 1);
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn clear_returns_everything_for_an_editor_restore() {
        let q = PendingQueue::new();
        q.enqueue(msg("typed while busy"));
        q.enqueue(msg("and this"));
        let cleared = q.clear();
        assert_eq!(cleared.len(), 2);
        assert!(q.is_empty());
        assert_eq!(q.texts(), Vec::<String>::new());
    }

    #[test]
    fn mode_parsing_is_forgiving_but_not_guessing() {
        assert_eq!(QueueMode::parse("all"), Some(QueueMode::All));
        assert_eq!(
            QueueMode::parse(" One-At-A-Time "),
            Some(QueueMode::OneAtATime)
        );
        assert_eq!(QueueMode::parse("single"), Some(QueueMode::OneAtATime));
        assert_eq!(QueueMode::parse("nonsense"), None);
    }

    #[test]
    fn snapshot_covers_both_queues() {
        let q = SteerQueues::new();
        q.steering.enqueue(msg("change direction"));
        q.follow_up.enqueue(msg("then summarise"));

        let snap = q.snapshot();
        assert_eq!(snap.steering, vec!["change direction".to_string()]);
        assert_eq!(snap.follow_up, vec!["then summarise".to_string()]);
        assert_eq!(snap.total(), 2);
        assert!(!snap.is_empty());
    }
}
