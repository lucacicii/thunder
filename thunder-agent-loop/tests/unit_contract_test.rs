use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

struct CountingClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for CountingClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let n = self.turn.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                let _ = tx.send(Err("cancelled".into())).await;
                return;
            }
            if n == 0 {
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("calling add".into()),
                        tool_calls: vec![ToolCall::new_function("c1", "add", "{\"x\":1,\"y\":2}")],
                        finish_reason: "tool_calls".into(),
                        prompt_tokens: Some(4),
                        completion_tokens: Some(4),
                    }))
                    .await;
            } else {
                let _ = tx.send(Ok(LLMStreamChunk::Token("done".into()))).await;
                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some("done".into()),
                        tool_calls: vec![],
                        finish_reason: "stop".into(),
                        prompt_tokens: Some(4),
                        completion_tokens: Some(2),
                    }))
                    .await;
            }
        });
        Ok(rx)
    }
}

struct AddTool;

#[async_trait]
impl AgentTool for AddTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "add",
            "Add two numbers",
            json!({"type":"object","properties":{"x":{"type":"number"},"y":{"type":"number"}},"required":["x","y"]}),
        )
    }

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let x = args["x"].as_f64().ok_or("x")?;
        let y = args["y"].as_f64().ok_or("y")?;
        Ok(json!({"sum": x + y}).to_string())
    }
}

fn make_agent(id: &str) -> AgentLoop {
    let scratch = std::env::temp_dir().join(format!("thunder_unit_{}_{}", std::process::id(), id));
    let config = AgentConfig::new("test-model").with_scratchpad_dir(scratch);
    let mut agent = AgentLoop::new(config)
        .with_id(id)
        .with_custom_client(Arc::new(CountingClient {
            turn: AtomicUsize::new(0),
        }));
    agent.register_tool(Arc::new(AddTool));
    agent
}

#[tokio::test]
async fn two_units_run_side_by_side_and_demux_events() {
    let a = make_agent("planner");
    let b = make_agent("coder");

    assert_eq!(a.id(), "planner");
    assert_eq!(b.id(), "coder");
    assert_ne!(a.scratchpad().session_dir(), b.scratchpad().session_dir());

    let mut ha = a.start("plan", None).unwrap();
    let mut hb = b.start("code", None).unwrap();

    let mut seen_a = 0usize;
    let mut seen_b = 0usize;
    if let Some(rx) = ha.events() {
        while let Some(ev) = rx.recv().await {
            assert_eq!(ev.agent_id, "planner");
            seen_a += 1;
        }
    }
    if let Some(rx) = hb.events() {
        while let Some(ev) = rx.recv().await {
            assert_eq!(ev.agent_id, "coder");
            seen_b += 1;
        }
    }

    let ra = ha.join().await.unwrap();
    let rb = hb.join().await.unwrap();

    assert_eq!(ra.agent_id, "planner");
    assert_eq!(rb.agent_id, "coder");
    assert_eq!(ra.finish_reason, FinishReason::Done);
    assert_eq!(rb.finish_reason, FinishReason::Done);
    assert!(seen_a > 0);
    assert!(seen_b > 0);
    assert!(!a.is_running());
    assert!(!b.is_running());
}

#[tokio::test]
async fn second_start_on_busy_unit_is_already_running() {
    struct HangClient;

    #[async_trait]
    impl LLMClientTrait for HangClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx.send(Ok(LLMStreamChunk::Token("...".into()))).await;
                cancel_token.cancelled().await;
                let _ = tx.send(Err("cancelled".into())).await;
            });
            Ok(rx)
        }
    }

    let agent = AgentLoop::new(AgentConfig::new("test-model"))
        .with_id("solo")
        .with_custom_client(Arc::new(HangClient));

    let handle = agent.start("first", None).unwrap();
    let err = agent.start("second", None).unwrap_err();
    assert!(matches!(err, AgentError::AlreadyRunning { agent_id } if agent_id == "solo"));

    handle.cancel();
    let result = handle.join().await.unwrap();
    assert_eq!(result.finish_reason, FinishReason::Cancelled);
    assert!(!agent.is_running());
}

#[tokio::test]
async fn cancel_one_unit_does_not_stop_sibling() {
    struct SlowThenDone {
        delay_ms: u64,
    }

    #[async_trait]
    impl LLMClientTrait for SlowThenDone {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            cancel_token: CancellationToken,
        ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            let delay = self.delay_ms;
            tokio::spawn(async move {
                tokio::select! {
                    _ = cancel_token.cancelled() => {
                        let _ = tx.send(Err("cancelled".into())).await;
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => {
                        let _ = tx
                            .send(Ok(LLMStreamChunk::Completed {
                                content: Some("ok".into()),
                                tool_calls: vec![],
                                finish_reason: "stop".into(),
                                prompt_tokens: Some(1),
                                completion_tokens: Some(1),
                            }))
                            .await;
                    }
                }
            });
            Ok(rx)
        }
    }

    let a = AgentLoop::new(AgentConfig::new("test-model"))
        .with_id("a1")
        .with_custom_client(Arc::new(SlowThenDone { delay_ms: 5_000 }));
    let b = AgentLoop::new(AgentConfig::new("test-model"))
        .with_id("a2")
        .with_custom_client(Arc::new(SlowThenDone { delay_ms: 20 }));

    let ha = a.start("hang", None).unwrap();
    let hb = b.start("finish", None).unwrap();

    ha.cancel();
    let ra = ha.join().await.unwrap();
    let rb = hb.join().await.unwrap();

    assert_eq!(ra.finish_reason, FinishReason::Cancelled);
    assert_eq!(rb.finish_reason, FinishReason::Done);
    assert_eq!(rb.final_content.as_deref(), Some("ok"));
}
