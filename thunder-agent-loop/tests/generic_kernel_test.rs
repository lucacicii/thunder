//! The kernel is a generic agent core, not a coding agent.
//!
//! This test is the guard rail for that claim: it runs a full closed loop with
//! **no file tools, no shell, no workspace** — a form submission workflow — and
//! exercises the three extension points that make the loop business-agnostic:
//!
//! * a tool that declares its own [`ToolEffect`] instead of relying on its name;
//! * a tool that emits a business event through the execution context's sink;
//! * a [`CompletionGate`] that refuses the first answer and asks for another.
//!
//! If any of this ever requires a coding concept to work, the kernel has stopped
//! being generic and this test is where that should surface.

use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::loop_engine::gate::{CompletionGate, GateRequest, GateVerdict};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::event::{AgentEvent, FinishReason};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::policy::ToolEffect;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// A business tool: it records a form submission. It has no filesystem
/// semantics at all, and it declares the effect it actually has.
struct SubmitFormTool {
    calls: Arc<Mutex<Vec<serde_json::Value>>>,
}

#[async_trait]
impl AgentTool for SubmitFormTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "submit_form",
            "Submit the collected form fields",
            json!({"type": "object", "properties": {"amount": {"type": "number"}}}),
        )
    }

    fn effect(&self) -> ToolEffect {
        // A submission mutates business state; it is not "exec" and not "read".
        ToolEffect::Write
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        self.calls.lock().await.push(args.clone());
        // Business-defined event: the kernel does not know what a "form_submitted"
        // is, it only carries it to the host.
        if let Some(sink) = ctx.event_sink.as_ref() {
            sink.emit_custom("form_submitted", json!({"fields": args}));
        }
        Ok("submitted".to_string())
    }
}

/// Scripted model: submit a form once, then claim to be done.
struct ScriptedClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for ScriptedClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                let _ = tx.send(Err("Cancelled".to_string())).await;
                return;
            }
            let chunk = if turn == 0 {
                LLMStreamChunk::Completed {
                    content: Some("Submitting the form.".to_string()),
                    tool_calls: vec![ToolCall::new_function(
                        "call_1",
                        "submit_form",
                        json!({"amount": 10}).to_string(),
                    )],
                    finish_reason: "tool_calls".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else if turn == 1 {
                // Claims to be finished before the gate has had a say; the gate
                // should send this back with feedback.
                LLMStreamChunk::Completed {
                    content: Some("The form is submitted.".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
                    cached_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                }
            } else {
                LLMStreamChunk::Completed {
                    content: Some(format!("Done after {turn} turn(s).")),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(5),
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

/// A gate that insists the amount must be over 100 before the run may finish.
struct MinimumAmountGate;

#[async_trait]
impl CompletionGate for MinimumAmountGate {
    async fn verify(&self, request: GateRequest) -> GateVerdict {
        if request.final_text.contains("Done after") {
            GateVerdict::Pass
        } else {
            GateVerdict::Retry {
                feedback: "the amount must be positive before you finish".to_string(),
            }
        }
    }
}

#[tokio::test]
async fn runs_a_non_code_workflow_end_to_end() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut agent = AgentLoop::new(
        AgentConfig::new("mock-model")
            .with_system_prompt("You collect and submit forms.")
            .with_max_turns(6),
    )
    .with_custom_client(Arc::new(ScriptedClient {
        turn: AtomicUsize::new(0),
    }))
    .with_completion_gate(Arc::new(MinimumAmountGate), 3);
    agent.register_tool(Arc::new(SubmitFormTool {
        calls: calls.clone(),
    }));

    let mut handle = agent.start("collect the form", None).unwrap();
    let mut saw_custom_event = false;
    let mut gate_retries = 0;
    if let Some(mut rx) = handle.take_events() {
        while let Some(observed) = rx.recv().await {
            match observed.event {
                AgentEvent::Custom { kind, .. } if kind == "form_submitted" => {
                    saw_custom_event = true;
                }
                AgentEvent::GateResult { verdict, .. } if verdict == "retry" => {
                    gate_retries += 1;
                }
                _ => {}
            }
        }
    }
    let result = handle.join().await.unwrap();

    assert_eq!(result.finish_reason, FinishReason::Done);
    assert_eq!(calls.lock().await.len(), 1, "the tool ran exactly once");
    assert!(saw_custom_event, "a tool-emitted business event was carried");
    assert_eq!(gate_retries, 1, "the gate sent the run back once");
    assert!(result
        .messages
        .iter()
        .any(|m| m.content_str().unwrap_or_default().contains("must be positive")));
}
