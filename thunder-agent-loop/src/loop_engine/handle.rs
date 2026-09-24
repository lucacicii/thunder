use crate::core::pause::PauseGate;
use crate::core::state::LoopStatus;
use crate::loop_engine::engine::AgentRunResult;
use crate::types::error::AgentError;
use crate::types::event::ObservedEvent;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Live handle for one in-flight unit execution.
///
/// Obtained from [`crate::AgentLoop::start`]. One handle = one task.
/// Dropping the handle does **not** cancel the loop; call [`Self::cancel`].
#[derive(Debug)]
pub struct AgentHandle {
    agent_id: String,
    status: Arc<AtomicU8>,
    running: Arc<AtomicBool>,
    cancel: CancellationToken,
    pause_gate: Arc<PauseGate>,
    result_rx: oneshot::Receiver<AgentRunResult>,
    event_rx: Option<mpsc::Receiver<ObservedEvent>>,
}

impl AgentHandle {
    pub(crate) fn new(
        agent_id: String,
        status: Arc<AtomicU8>,
        running: Arc<AtomicBool>,
        cancel: CancellationToken,
        pause_gate: Arc<PauseGate>,
        result_rx: oneshot::Receiver<AgentRunResult>,
        event_rx: mpsc::Receiver<ObservedEvent>,
    ) -> Self {
        Self {
            agent_id,
            status,
            running,
            cancel,
            pause_gate,
            result_rx,
            event_rx: Some(event_rx),
        }
    }

    /// Request a cooperative pause. Takes effect at the next tool boundary, so
    /// an in-flight tool completes rather than being interrupted.
    pub fn pause(&self) {
        self.pause_gate.pause();
    }

    /// Release a pause and let the loop advance.
    pub fn resume(&self) {
        self.pause_gate.resume();
    }

    pub fn is_paused(&self) -> bool {
        self.pause_gate.is_paused()
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn status(&self) -> LoopStatus {
        LoopStatus::from_u8(self.status.load(Ordering::Acquire))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Reliable per-run event stream. The loop applies backpressure when this
    /// channel fills — events are not silently dropped.
    ///
    /// Returns `None` after the receiver has been taken (e.g. by [`Self::take_events`]).
    pub fn events(&mut self) -> Option<&mut mpsc::Receiver<ObservedEvent>> {
        self.event_rx.as_mut()
    }

    /// Take the event receiver so the caller can move it into another task.
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<ObservedEvent>> {
        self.event_rx.take()
    }

    /// Cancel this unit's in-flight task. Sibling agents are unaffected.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Wait until the closed loop finishes and return its aggregated result.
    pub async fn join(self) -> Result<AgentRunResult, AgentError> {
        match self.result_rx.await {
            Ok(result) => Ok(result),
            Err(_) => Err(AgentError::Terminated {
                agent_id: self.agent_id,
            }),
        }
    }
}

/// Clears the unit's busy flag even if the spawned loop panics.
pub(crate) struct RunningGuard {
    running: Arc<AtomicBool>,
}

impl RunningGuard {
    pub(crate) fn new(running: Arc<AtomicBool>) -> Self {
        Self { running }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
    }
}
