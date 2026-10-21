//! KERNEL.md §8 anchor 13: a finished run holds no state.
//!
//! Twice now a shared handle has outlived the run that installed it: the trace
//! sink left in a host's executor kept the event sender alive, and every host
//! that waited for the stream to close hung. The counters below are the cheap
//! proxy for "nothing of the run survived it": the loop is the only owner of
//! its state once the task ends.

use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::FinishReason;
use tokio_util::sync::CancellationToken;

struct OneShot;

#[async_trait]
impl LLMClientTrait for OneShot {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("done".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

/// A host that holds nothing but the agent sees the stream close and every
/// counter return to its single owner.
#[tokio::test]
async fn a_finished_run_holds_no_state() {
    let agent = AgentLoop::new(AgentConfig::new("test-model").with_max_turns(2))
        .with_custom_client(Arc::new(OneShot));
    let drops = Arc::clone(agent.event_drop_counters());
    // The agent and this test are the only legitimate owners once the run ends.
    let baseline = Arc::strong_count(&drops);

    let mut handle = agent.start("go", None).expect("start");
    let mut events = handle.take_events().expect("stream");

    let mut seen = 0usize;
    while events.recv().await.is_some() {
        seen += 1;
    }
    assert!(seen > 0, "the run must have reported something");

    let result = handle.join().await.expect("run");
    assert_eq!(result.finish_reason, FinishReason::Done);

    assert_eq!(
        Arc::strong_count(&drops),
        baseline,
        "a run handle outlived the run: {} owners remain, {} before it started",
        Arc::strong_count(&drops),
        baseline
    );
}

/// The stream ends on its own: no consumer has to close it, and no sink keeps
/// the sender alive. This is the property the trace-sink leak broke.
#[tokio::test]
async fn the_stream_closes_when_the_run_ends() {
    let agent = AgentLoop::new(AgentConfig::new("test-model").with_max_turns(2))
        .with_custom_client(Arc::new(OneShot));

    let mut handle = agent.start("go", None).expect("start");
    let mut events = handle.take_events().expect("stream");
    let _ = handle.join().await.expect("run");

    // `join` means the *result* is ready; the stream may still hold its tail.
    // What must hold is that the tail ends on its own, with no producer left.
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while events.recv().await.is_some() {}
    })
    .await;
    assert!(
        ended.is_ok(),
        "the stream must end with the run, not wait for a producer that is gone"
    );
}

/// A dropped handle cancels nothing (that is `cancel`'s job) but must not keep
/// the run alive either: the task finishes and its counters return.
#[tokio::test]
async fn dropping_the_handle_does_not_pin_the_run() {
    let agent = AgentLoop::new(AgentConfig::new("test-model").with_max_turns(1))
        .with_custom_client(Arc::new(OneShot));
    let drops = Arc::clone(agent.event_drop_counters());
    let baseline = Arc::strong_count(&drops);

    let mut handle = agent.start("go", None).expect("start");
    let mut events = handle.take_events().expect("stream");
    let _ = handle.join().await.expect("run");
    while events.recv().await.is_some() {}

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while Arc::strong_count(&drops) > baseline && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        Arc::strong_count(&drops),
        baseline,
        "the run task must release its counters when it ends"
    );
}
