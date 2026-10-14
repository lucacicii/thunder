//! The execution-status plugin on a real run's event stream.
//!
//! Unit coverage of the phase machine lives in `plugins::status`; what this file
//! checks is the part only a real run can show: that a host receives
//! `custom/agent_status` frames on the same stream as everything else, that they
//! arrive only when the status actually moves, and — the property the plugin's
//! change signal exists for — that a host-reported wait reaches the consumer
//! *while the run is parked with no kernel event in flight*.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

/// Answers the first turn with a bash call, then parks until the test releases
/// it — so in between, the loop is waiting on the model and emits nothing.
struct ParkingMockClient {
    turn: AtomicUsize,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl LLMClientTrait for ParkingMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        let release = Arc::clone(&self.release);

        tokio::spawn(async move {
            let chunk = if turn == 0 {
                LLMStreamChunk::Completed {
                    content: Some("Let me check the directory.\n".to_string()),
                    tool_calls: vec![ToolCall::new_function(
                        "call_1",
                        "bash",
                        "{\"command\":\"echo status test\"}",
                    )],
                    finish_reason: "tool_calls".to_string(),
                    prompt_tokens: Some(options.messages.len() * 10),
                    completion_tokens: Some(20),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else {
                // Park here until the test has seen the injected wait; the run
                // publishes nothing of its own in the meantime.
                release.notified().await;
                LLMStreamChunk::Completed {
                    content: Some("Status test done.".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(options.messages.len() * 10),
                    completion_tokens: Some(15),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            };
            let _ = tx.send(Ok(chunk)).await;
        });

        Ok(rx)
    }
}

/// The status frame carried by an event, when it is one.
fn status_of(event: &ObservedEvent) -> Option<AgentStatusSnapshot> {
    let AgentEvent::Custom { kind, payload } = &event.event else {
        return None;
    };
    if kind != AGENT_STATUS_EVENT_KIND {
        return None;
    }
    serde_json::from_value::<AgentStatusSnapshot>(payload.clone()).ok()
}

#[tokio::test]
async fn a_run_publishes_its_status_and_reports_a_host_reported_wait_without_an_event() {
    let status = StatusPlugin::new();
    let root = ThunderRoot::new(AgentConfig::new("gpt-4o").with_unlimited_turns())
        .with_plugin(ConversationPlugin::with_memory_store())
        .with_status_plugin(status.clone());

    let session_id = "test_status_sess_1".to_string();
    let release = Arc::new(tokio::sync::Notify::new());
    let client: Arc<dyn LLMClientTrait> = Arc::new(ParkingMockClient {
        turn: AtomicUsize::new(0),
        release: Arc::clone(&release),
    });

    let options = RootRunOptions {
        session_id: Some(session_id.clone()),
        custom_client: Some(client),
        ..Default::default()
    };

    let mut handle = root
        .execute("check the directory", options)
        .await
        .expect("root execute");
    let mut rx = handle.take_events().expect("event stream");

    let mut phases: Vec<AgentPhase> = Vec::new();
    let mut tool_names: Vec<String> = Vec::new();
    // Everything a consumer renders, so "did this frame change anything?" can be
    // asked of the frame rather than of its phase alone.
    let mut frames: Vec<(AgentPhase, Option<String>, usize)> = Vec::new();
    let mut event_count = 0usize;
    let mut injected = false;
    let mut released = false;

    while let Some(event) = rx.recv().await {
        event_count += 1;
        if let Some(snapshot) = status_of(&event) {
            if let Some(tool) = snapshot.tool.clone() {
                tool_names.push(tool);
            }
            frames.push((snapshot.phase, snapshot.tool, snapshot.turn));
            phases.push(snapshot.phase);
        }

        // Once the tool result has been seen, the loop is back on the model —
        // which is parked. From here on, only the plugin's own change signal can
        // produce another status frame.
        if !injected && matches!(&event.event, AgentEvent::ToolExecResult { .. }) {
            injected = true;
            assert!(status.note_awaiting(&session_id, true));
        }

        if injected && !released && phases.last() == Some(&AgentPhase::AwaitingUser) {
            released = true;
            assert!(status.note_awaiting(&session_id, false));
            release.notify_one();
        }
    }

    assert!(
        injected,
        "the run never executed its tool, so the wait could not be injected"
    );
    assert_eq!(
        phases.last(),
        Some(&AgentPhase::Done),
        "a run that ends cleanly publishes `done` last: {phases:?}"
    );
    assert!(
        phases.contains(&AgentPhase::AwaitingUser),
        "a host-reported wait must reach the consumer while the run is parked: {phases:?}"
    );
    assert!(
        phases.contains(&AgentPhase::Tool),
        "the tool phase must be published: {phases:?}"
    );
    assert_eq!(
        tool_names.first().map(String::as_str),
        Some("bash"),
        "the tool phase must name the tool being executed: {tool_names:?}"
    );
    assert!(
        phases.contains(&AgentPhase::Thinking),
        "the run must return to `thinking` once the tool batch lands: {phases:?}"
    );

    // Frames are transition-driven, so a token stream never looks like a state
    // change: no frame may repeat the previous one's status in full.
    for pair in frames.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "a status frame republished a status it had already sent: {frames:?}"
        );
    }
    assert!(
        frames.len() < event_count,
        "status frames must be a strict subset of the run's events \
         ({} frames out of {event_count} events)",
        frames.len()
    );

    assert!(matches!(
        handle.join().await.expect("join").run_result.finish_reason,
        FinishReason::Done
    ));
}
