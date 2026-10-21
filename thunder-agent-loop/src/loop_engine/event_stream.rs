//! A byte budget for one run's event queue.
//!
//! The channel is bounded by *count* (512 frames), which bounds memory only if
//! every frame is the same size — and they are not: one `ToolExecResult` can be
//! as large as `max_output_bytes`. A consumer that stops reading therefore pins
//! `512 × output_cap` bytes, not `512 × a few hundred`.
//!
//! The budget makes the bound explicit: producers reserve the event's estimated
//! size before enqueueing, and the consumer releases it on `recv`. A producer
//! that cannot get space within [`DELIVERY_DEADLINE`] drops the event and counts
//! it, which is the same contract the emitter already had for a full channel.
//!
//! One event larger than the whole budget can never fit; it is dropped with a
//! warning rather than silently truncating what a host would have recorded.

use crate::types::event::{EventDropCounters, ObservedEvent};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Semaphore, SemaphorePermit};

/// How long a producer waits for queue space before dropping the event.
///
/// The same deadline the emitter used for a full channel: a consumer that has
/// not moved in five seconds is not going to, and a run must not park forever
/// on a host that stopped reading.
pub const DELIVERY_DEADLINE: Duration = Duration::from_secs(5);

/// Default ceiling on queued event bytes for one run.
///
/// Below the pathological `512 × 64 KB` (32 MB) a count-bounded channel allows,
/// and above a normal run's steady state (a few hundred small frames).
pub const DEFAULT_MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// The run's queue budget, shared by every producer and its consumer.
#[derive(Debug)]
pub struct EventBudget {
    permits: Arc<Semaphore>,
    max: usize,
}

impl EventBudget {
    pub fn new(max_bytes: usize) -> Self {
        // A budget below one frame would drop everything; clamp rather than
        // accept a configuration that silently disables the stream.
        let max = max_bytes.max(1024);
        Self {
            permits: Arc::new(Semaphore::new(max)),
            max,
        }
    }

    /// The largest event that can ever be queued.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Reserve `bytes` of queue space, waiting up to [`DELIVERY_DEADLINE`].
    ///
    /// `None` means the event was dropped: either it can never fit, or the
    /// consumer has not made room. The permit is forgotten by the producer on a
    /// successful send; the consumer releases it on `recv`.
    pub async fn reserve(&self, bytes: usize) -> Option<SemaphorePermit<'_>> {
        if bytes > self.max {
            return None;
        }
        match tokio::time::timeout(DELIVERY_DEADLINE, self.permits.acquire_many(bytes as u32)).await
        {
            Ok(Ok(permit)) => Some(permit),
            _ => None,
        }
    }

    /// Reserve without waiting. Used by the synchronous sink paths.
    pub fn try_reserve(&self, bytes: usize) -> Option<SemaphorePermit<'_>> {
        if bytes > self.max {
            return None;
        }
        self.permits.try_acquire_many(bytes as u32).ok()
    }

    /// Return space to the budget after an event left the queue.
    pub fn release(&self, bytes: usize) {
        self.permits.add_permits(bytes.min(self.max));
    }
}

/// The consumer half of a run's event stream.
///
/// A thin wrapper over the channel so releasing the budget cannot be forgotten:
/// every event that leaves the queue gives its bytes back exactly once.
#[derive(Debug)]
pub struct EventStream {
    rx: mpsc::Receiver<ObservedEvent>,
    budget: Arc<EventBudget>,
}

impl EventStream {
    pub fn new(rx: mpsc::Receiver<ObservedEvent>, budget: Arc<EventBudget>) -> Self {
        Self { rx, budget }
    }

    /// The next event, or `None` once the loop has dropped its sender.
    pub async fn recv(&mut self) -> Option<ObservedEvent> {
        let event = self.rx.recv().await?;
        self.budget.release(event.approx_bytes());
        Some(event)
    }

    pub fn try_recv(&mut self) -> Result<ObservedEvent, mpsc::error::TryRecvError> {
        let event = self.rx.try_recv()?;
        self.budget.release(event.approx_bytes());
        Ok(event)
    }

    /// Stop accepting new events; `recv` drains what is already queued.
    pub fn close(&mut self) {
        self.rx.close();
    }

    /// How many frames fit in the channel right now (not bytes).
    pub fn capacity(&self) -> usize {
        self.rx.capacity()
    }
}

/// Deliver one event under the budget, or count the loss.
///
/// Shared by the emitter and both sinks so there is one place that decides
/// between "queued", "dropped as oversized" and "dropped after the deadline".
pub async fn deliver(
    sender: &mpsc::Sender<ObservedEvent>,
    budget: &EventBudget,
    drops: &EventDropCounters,
    event: ObservedEvent,
    micro: bool,
) {
    let bytes = event.approx_bytes();
    let Some(permit) = budget.try_reserve(bytes) else {
        // Either it can never fit (counted as structural: a host's record is
        // missing a frame it would have wanted) or the queue is full right now.
        if bytes > budget.max() {
            drops.record_structural();
            tracing::warn!(
                bytes,
                budget = budget.max(),
                "Event larger than the run's whole event budget; dropped"
            );
            return;
        }
        // Full right now: micro-deltas are lossy by contract, everything else
        // gets the deadline.
        if micro {
            drops.record_micro();
            return;
        }
        let Some(permit) = budget.reserve(bytes).await else {
            drops.record_structural();
            tracing::warn!(bytes, "Event dropped: no queue space before the deadline");
            return;
        };
        if sender.send(event).await.is_ok() {
            permit.forget();
        } else {
            drops.record_structural();
        }
        return;
    };

    if micro {
        match sender.try_send(event) {
            Ok(()) => permit.forget(),
            Err(_) => drops.record_micro(),
        }
        return;
    }

    match tokio::time::timeout(DELIVERY_DEADLINE, sender.send(event)).await {
        Ok(Ok(())) => permit.forget(),
        _ => drops.record_structural(),
    }
}
