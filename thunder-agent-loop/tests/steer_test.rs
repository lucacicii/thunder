//! Steering and follow-up queues, end to end through the real agent loop.
//!
//! The property under test is *when* a queued user message enters the run:
//! steering at the next turn boundary (and able to keep a concluding run
//! alive), follow-up only once nothing else is left to do — and never in the
//! middle of a tool.

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};

/// One scripted assistant response.
struct TurnScript {
    content: &'static str,
    tool_calls: Vec<ToolCall>,
    finish_reason: &'static str,
}

fn says(content: &'static str) -> TurnScript {
    TurnScript {
        content,
        tool_calls: Vec::new(),
        finish_reason: "stop",
    }
}

/// A client that records every request and can queue input *while* the loop is
/// running, which is the only way to test real mid-run steering deterministically.
struct ScriptedClient {
    script: Vec<TurnScript>,
    calls: AtomicUsize,
    /// User-message texts of each request, in order.
    requests: Mutex<Vec<Vec<String>>>,
    /// Image counts per user message, parallel to `requests`.
    request_images: Mutex<Vec<Vec<usize>>>,
    queues: Arc<SteerQueues>,
    /// `(after_call_index, behavior, text)` — enqueued when that call arrives.
    inject: Mutex<Vec<(usize, QueueBehavior, &'static str)>>,
}

impl ScriptedClient {
    fn new(script: Vec<TurnScript>, queues: Arc<SteerQueues>) -> Arc<Self> {
        Arc::new(Self {
            script,
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            request_images: Mutex::new(Vec::new()),
            queues,
            inject: Mutex::new(Vec::new()),
        })
    }

    /// Queue input to arrive while the loop is executing the given call.
    fn inject_after(&self, call: usize, behavior: QueueBehavior, text: &'static str) {
        self.inject.lock().push((call, behavior, text));
    }

    fn request_texts(&self) -> Vec<Vec<String>> {
        self.requests.lock().clone()
    }

    /// Image counts per user message, one vec per request.
    fn request_image_counts(&self) -> Vec<Vec<usize>> {
        self.request_images.lock().clone()
    }

    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMClientTrait for ScriptedClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);

        let users: Vec<String> = options
            .messages
            .iter()
            .filter(|m| matches!(m, ChatMessage::User { .. }))
            .filter_map(|m| m.content_str().map(str::to_string))
            .collect();
        self.requests.lock().push(users);
        self.request_images.lock().push(
            options
                .messages
                .iter()
                .filter(|m| matches!(m, ChatMessage::User { .. }))
                .map(|m| m.image_count())
                .collect(),
        );

        // Queue anything scheduled for "while this request is in flight".
        let scheduled: Vec<(usize, QueueBehavior, &'static str)> =
            self.inject.lock().iter().copied().collect();
        for (after, behavior, text) in scheduled {
            if after == call {
                let message = ChatMessage::user(text);
                match behavior {
                    QueueBehavior::Steer => self.queues.steering.enqueue(message),
                    QueueBehavior::FollowUp => self.queues.follow_up.enqueue(message),
                }
            }
        }

        let scripted = self
            .script
            .get(call)
            .or_else(|| self.script.last())
            .expect("a scripted turn");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let (content, tool_calls, finish_reason) = (
            scripted.content.to_string(),
            scripted.tool_calls.clone(),
            scripted.finish_reason.to_string(),
        );
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(content),
                    tool_calls,
                    finish_reason,
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

/// Counts executions, so a test can prove a tool was not interrupted.
struct CountingTool {
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentTool for CountingTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("probe", "counts", json!({"type": "object"}))
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok("probed".to_string())
    }
}

/// Builds an agent wired to a shared queue pair and a scripted client.
fn agent_with(queues: Arc<SteerQueues>, client: Arc<ScriptedClient>) -> AgentLoop {
    AgentLoop::new(AgentConfig::new("mock").with_max_turns(8))
        .with_steer_queues(Arc::clone(&queues))
        .with_custom_client(client)
}

#[tokio::test]
async fn a_steering_message_keeps_a_concluding_run_alive() {
    let queues = SteerQueues::new_shared();
    // Both turns conclude on their own; without the queued steer the run would
    // stop after the first.
    let client = ScriptedClient::new(vec![says("first"), says("second")], Arc::clone(&queues));
    client.inject_after(0, QueueBehavior::Steer, "actually, change of plan");
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    let result = tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(client.call_count(), 2, "the steer bought a second turn");

    let requests = client.request_texts();
    assert_eq!(requests[0], vec!["go".to_string()]);
    assert_eq!(
        requests[1],
        vec!["go".to_string(), "actually, change of plan".to_string()],
        "the steer enters the next request as an ordinary user message"
    );
    assert!(queues.steering.is_empty(), "the queue was drained");
}

#[tokio::test]
async fn one_at_a_time_delivers_one_message_per_turn() {
    let queues = SteerQueues::new_shared();
    // Default mode is one-at-a-time.
    assert_eq!(queues.steering.mode(), QueueMode::OneAtATime);

    let client = ScriptedClient::new(
        vec![says("one"), says("two"), says("three")],
        Arc::clone(&queues),
    );
    client.inject_after(0, QueueBehavior::Steer, "second instruction");
    client.inject_after(0, QueueBehavior::Steer, "third instruction");
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    let result = tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    assert_eq!(result.finish_reason, FinishReason::Done);
    let requests = client.request_texts();
    assert_eq!(requests.len(), 3, "one extra turn per queued message");

    // Each queued message arrives on its own turn, in order.
    assert!(!requests[1].contains(&"third instruction".to_string()));
    assert!(requests[1].contains(&"second instruction".to_string()));
    assert!(requests[2].contains(&"third instruction".to_string()));
}

#[tokio::test]
async fn all_mode_delivers_the_batch_together() {
    let queues = SteerQueues::new_shared();
    queues.steering.set_mode(QueueMode::All);

    let client = ScriptedClient::new(vec![says("one"), says("two")], Arc::clone(&queues));
    client.inject_after(0, QueueBehavior::Steer, "second instruction");
    client.inject_after(0, QueueBehavior::Steer, "third instruction");
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    let requests = client.request_texts();
    assert_eq!(requests.len(), 2, "both arrive in the same turn");
    assert!(requests[1].contains(&"second instruction".to_string()));
    assert!(requests[1].contains(&"third instruction".to_string()));
}

#[tokio::test]
async fn a_follow_up_waits_until_nothing_else_is_left_to_do() {
    let queues = SteerQueues::new_shared();
    // Queued before the run starts: the steer enters turn one, the follow-up
    // only after that turn concludes.
    queues
        .steering
        .enqueue(ChatMessage::user("do this instead"));
    queues
        .follow_up
        .enqueue(ChatMessage::user("and then summarise"));

    let client = ScriptedClient::new(vec![says("one"), says("two")], Arc::clone(&queues));
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    let requests = client.request_texts();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0].contains(&"do this instead".to_string()),
        "steering goes first: {:?}",
        requests[0]
    );
    assert!(
        !requests[0].contains(&"and then summarise".to_string()),
        "the follow-up must not ride along with the steer"
    );
    assert!(
        requests[1].contains(&"and then summarise".to_string()),
        "the follow-up gets its own turn: {:?}",
        requests[1]
    );
}

#[tokio::test]
async fn steering_never_interrupts_an_in_flight_tool() {
    let runs = Arc::new(AtomicUsize::new(0));
    let queues = SteerQueues::new_shared();

    // Turn one asks for a tool; the steer is queued while that request is in
    // flight, so it must be delivered *after* the tool has run.
    let client = ScriptedClient::new(
        vec![
            TurnScript {
                content: "working",
                tool_calls: vec![ToolCall::new_function("call_1", "probe", "{}")],
                finish_reason: "tool_calls",
            },
            says("done"),
        ],
        Arc::clone(&queues),
    );
    client.inject_after(0, QueueBehavior::Steer, "hold on");

    let mut agent = agent_with(Arc::clone(&queues), Arc::clone(&client));
    agent.register_tool(Arc::new(CountingTool {
        runs: Arc::clone(&runs),
    }));

    let result = tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the requested tool ran exactly once, uninterrupted"
    );

    let requests = client.request_texts();
    assert!(
        !requests[0].contains(&"hold on".to_string()),
        "the steer must not appear in the request it was queued during"
    );
    assert!(
        requests[1].contains(&"hold on".to_string()),
        "it lands in the turn after the tools: {:?}",
        requests[1]
    );
}

#[tokio::test]
async fn a_steering_message_carries_its_images() {
    let queues = SteerQueues::new_shared();
    // Queued before the run, so the very first request carries it.
    queues.steering.enqueue(ChatMessage::user_multimodal(
        "look at this instead",
        vec![ContentPart::image("image/png", "aGVsbG8=")],
    ));

    let client = ScriptedClient::new(vec![says("ok")], Arc::clone(&queues));
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    let images = client.request_image_counts();
    assert_eq!(
        images[0],
        vec![0, 1],
        "`go` carries none; the queued message carries its image"
    );
}

#[tokio::test]
async fn steer_accepted_events_report_what_entered_the_run() {
    let queues = SteerQueues::new_shared();
    let client = ScriptedClient::new(vec![says("one"), says("two")], Arc::clone(&queues));
    client.inject_after(0, QueueBehavior::Steer, "new direction");
    let agent = agent_with(Arc::clone(&queues), Arc::clone(&client));

    // Subscribe before the run so nothing is missed.
    let mut events = agent.subscribe_events();
    tokio::time::timeout(Duration::from_secs(5), agent.run("go", None))
        .await
        .expect("run finishes")
        .expect("no error");

    let mut accepted = Vec::new();
    while let Ok(observed) = events.try_recv() {
        if let AgentEvent::SteerAccepted {
            behavior, message, ..
        } = observed.event
        {
            accepted.push((behavior, message));
        }
    }
    assert_eq!(
        accepted,
        vec![("steer".to_string(), "new direction".to_string())],
        "the host is told exactly what entered, and from which queue"
    );
}

#[tokio::test]
async fn clear_queue_hands_the_text_back() {
    let queues = SteerQueues::new_shared();
    queues
        .steering
        .enqueue(ChatMessage::user("typed while busy"));
    queues.follow_up.enqueue(ChatMessage::user("and this"));

    let (steering, follow_up) = queues.clear_all();
    assert_eq!(steering[0].content_str(), Some("typed while busy"));
    assert_eq!(follow_up[0].content_str(), Some("and this"));
    assert!(queues.snapshot().is_empty());
}
