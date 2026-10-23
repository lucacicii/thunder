//! Phase E: recovery level L1 — a stalled run rebuilds its context instead of dying.
//!
//! The recovery budget is shared with the run's turn/token/wall-clock budget and is
//! never recharged, so automatic recovery can never turn into an unbounded loop.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, FinishReason, LlmTurnResponse, LoopBudget, ReActAgentLoop, ScriptedMockDriver,
    StallConfig, ToolCallRequest,
};
use thunder_protocol::{
    AgentEvent, ChatMessage, EventSink, ExecutionContext, ProgressKind, ThunderEvent,
    ToolDescriptor, ToolExecutionError, ToolProvider,
};
use thunder_registry::ToolRegistry;

#[derive(Default)]
struct Sink {
    events: Mutex<Vec<AgentEvent>>,
}

impl EventSink for Sink {
    fn on_event(&self, _event: ThunderEvent) {}

    fn on_agent_event(&self, event: AgentEvent) {
        self.events.lock().unwrap().push(event);
    }
}

impl Sink {
    fn recovered(&self) -> Vec<Value> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Custom { kind, payload } if kind == "loop_recovered" => {
                    Some(payload.clone())
                }
                _ => None,
            })
            .collect()
    }
}

struct Provider;

#[async_trait]
impl ToolProvider for Provider {
    fn provider_id(&self) -> &str {
        "p"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_mcp(
            "p",
            "t",
            "t",
            json!({ "type": "object" }),
        )
        .with_progress(ProgressKind::Neutral)])
    }

    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        // Constant output: every repeated call is informationally empty.
        Ok("constant".to_string())
    }
}

fn config(ws: &std::path::Path, loop_budget: LoopBudget, stall: StallConfig) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-recovery".to_string(),
        workspace_cwd: ws.to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: thunder_core::ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(5),
        cache_warm_settings: None,
        budget: loop_budget,
        stall,
    }
}

fn call(id: &str, n: usize) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        name: "mcp__p__t".to_string(),
        arguments: json!({ "n": n }),
    }
}

async fn registry() -> ToolRegistry {
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(Provider))
        .await
        .unwrap();
    registry
}

#[tokio::test]
async fn a_stalled_run_resets_its_context_and_finishes() {
    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    // Three fruitless turns reach the stall threshold; after the reset the model gets a
    // fresh view and answers immediately.
    let mut steps: Vec<LlmTurnResponse> = (0..3)
        .map(|n| LlmTurnResponse::simple("spinning", vec![call(&format!("c{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple(
        "RECOVERED: here is the answer",
        vec![],
    ));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver.clone(),
    )
    .with_event_sink(sink.clone());

    let outcome = agent.run("find the bug").await.expect("run completes");
    assert_eq!(outcome.finish_reason, FinishReason::Done);
    assert_eq!(outcome.final_response, "RECOVERED: here is the answer");

    let recovered = sink.recovered();
    assert_eq!(recovered.len(), 1, "exactly one reset: {recovered:?}");
    assert_eq!(recovered[0]["level"], "context_reset");
    assert_eq!(recovered[0]["attempt"], 1);

    // The rebuilt context keeps the goal and carries the handoff, and holds no trace of
    // the stuck trajectory.
    let messages = agent.context().get_messages();
    assert!(
        messages.iter().any(|m| m
            .content_str()
            .map(|c| c.contains("<handoff>"))
            .unwrap_or(false)),
        "recovered context must carry a handoff block"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.content_str() == Some("find the bug")),
        "the original goal must survive the reset"
    );
    assert!(
        !messages.iter().any(|m| m
            .content_str()
            .map(|c| c.contains("spinning"))
            .unwrap_or(false)),
        "the stuck trajectory must be gone"
    );
}

#[tokio::test]
async fn recovery_budget_exhaustion_falls_back_to_a_wrap_up() {
    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    // Two stalls but only one reset allowed: the second stall wraps up.
    let mut steps: Vec<LlmTurnResponse> = Vec::new();
    for n in 0..6 {
        steps.push(LlmTurnResponse::simple(
            "spinning",
            vec![call(&format!("c{n}"), n)],
        ));
    }
    steps.push(LlmTurnResponse::simple("GAVE UP SUMMARY", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver,
    )
    .with_event_sink(sink.clone());

    let outcome = agent
        .run("spin forever")
        .await
        .expect("wrap-up is not an error");
    assert_eq!(outcome.finish_reason, FinishReason::BudgetExceeded);
    assert_eq!(sink.recovered().len(), 1, "only one reset was allowed");
    assert!(outcome.final_response.contains("GAVE UP SUMMARY"));
}

#[tokio::test]
async fn recovery_is_disabled_when_max_context_resets_is_zero() {
    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    let mut steps: Vec<LlmTurnResponse> = (0..4)
        .map(|n| LlmTurnResponse::simple("spinning", vec![call(&format!("c{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple("SUMMARY", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 0,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver,
    )
    .with_event_sink(sink.clone());

    let outcome = agent.run("spin").await.expect("wrap-up is not an error");
    assert!(sink.recovered().is_empty());
    assert_eq!(outcome.finish_reason, FinishReason::BudgetExceeded);
}

#[tokio::test]
async fn the_recovered_context_is_a_valid_provider_payload() {
    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    let mut steps: Vec<LlmTurnResponse> = (0..3)
        .map(|n| LlmTurnResponse::simple("spinning", vec![call(&format!("c{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple("done now", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver.clone(),
    )
    .with_event_sink(sink);

    agent.run("recover me").await.expect("run completes");

    // Every request the mock saw must be replayable: no assistant `tool_calls` without
    // its tool results.
    for request in driver.recorded_messages() {
        let mut pending: Vec<String> = Vec::new();
        for msg in &request {
            match msg {
                ChatMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => {
                    assert!(pending.is_empty(), "orphaned tool_calls before {pending:?}");
                    pending = calls.iter().map(|c| c.id.clone()).collect();
                }
                ChatMessage::Tool { tool_call_id, .. } if !pending.is_empty() => {
                    assert_eq!(pending.remove(0), *tool_call_id);
                }
                _ => {}
            }
        }
        assert!(pending.is_empty(), "unpaired tool_calls: {pending:?}");
    }
}

#[tokio::test]
async fn reset_counters_keep_the_run_budget_honest() {
    let ws = tempdir().unwrap();
    let mut steps: Vec<LlmTurnResponse> = (0..3)
        .map(|n| LlmTurnResponse::simple("spinning", vec![call(&format!("c{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple("after reset", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver,
    );

    let outcome = agent.run("recover").await.expect("run completes");
    assert_eq!(outcome.finish_reason, FinishReason::Done);
    // Turn accounting is continuous across the reset: no recharge.
    assert_eq!(
        outcome.stats.total_turns, 4,
        "three stalled turns plus the recovered turn"
    );
}

/// Same tool name as [`Provider`], but the plugin declares it as a writer.
struct MutatingProvider;

#[async_trait]
impl ToolProvider for MutatingProvider {
    fn provider_id(&self) -> &str {
        "p"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_mcp(
            "p",
            "t",
            "t",
            json!({ "type": "object" }),
        )
        .with_progress(ProgressKind::Mutating)])
    }

    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        Ok("constant".to_string())
    }
}

#[tokio::test]
async fn the_handoff_uses_the_declared_progress_kind_not_the_tool_name() {
    // `mcp__p__t` looks nothing like a write tool, but the plugin declares Mutating. The
    // recovery handoff must list its path under "already modified" — the previous
    // name-sniffing heuristic would have filed it as a read.
    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    let mut steps: Vec<LlmTurnResponse> = (0..3)
        .map(|n| {
            LlmTurnResponse::simple(
                "spinning",
                vec![ToolCallRequest {
                    id: format!("c{n}"),
                    name: "mcp__p__t".to_string(),
                    arguments: json!({ "path": "src/x.rs" }),
                }],
            )
        })
        .collect();
    steps.push(LlmTurnResponse::simple("done", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        {
            let registry = ToolRegistry::new();
            registry
                .register_provider(Arc::new(MutatingProvider))
                .await
                .unwrap();
            registry
        },
        driver,
    )
    .with_event_sink(sink.clone());

    agent.run("edit src/x.rs").await.expect("run completes");

    let handoff = agent
        .context()
        .get_messages()
        .iter()
        .filter_map(|m| m.content_str())
        .find(|c| c.contains("<handoff>"))
        .expect("handoff present")
        .to_string();
    let modified_section = handoff
        .split("Files already modified")
        .nth(1)
        .unwrap_or_default();
    assert!(
        modified_section.contains("src/x.rs"),
        "declared Mutating must land in the modified list, handoff was:\n{handoff}"
    );
}

/// A recovered run is still the same question about the same image.
///
/// Recovery rebuilds the context from the goal, so `goal` alone is not enough: the
/// attached image has to be restored too, or the agent silently starts answering a
/// different prompt after the reset.
#[tokio::test]
async fn a_context_reset_keeps_the_attached_image() {
    use thunder_protocol::ContentPart;

    let ws = tempdir().unwrap();
    let sink = Arc::new(Sink::default());

    let mut steps: Vec<LlmTurnResponse> = (0..3)
        .map(|n| LlmTurnResponse::simple("spinning", vec![call(&format!("i{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple("RECOVERED: it is blue", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 20,
                wall_clock: None,
                max_total_tokens: 0,
                max_context_resets: 1,
                ..LoopBudget::default()
            },
            StallConfig {
                mutation_free_turns: 100,
                no_novel_turns: 2,
            },
        ),
        registry().await,
        driver,
    )
    .with_event_sink(sink.clone());

    let prompt = "what colour is this screenshot?";
    let outcome = agent
        .run_with_parts(
            prompt,
            vec![
                ContentPart::text(prompt),
                ContentPart::image("image/png", "aGVsbG8="),
            ],
        )
        .await
        .expect("run completes");

    assert_eq!(outcome.finish_reason, FinishReason::Done);
    assert_eq!(sink.recovered().len(), 1, "the run must have reset once");

    let messages = agent.context().get_messages();
    let restored = messages
        .iter()
        .find(|message| message.image_count() == 1)
        .expect("the image must survive the context reset");
    assert_eq!(
        restored
            .image_parts()
            .next()
            .and_then(|part| part.as_image()),
        Some(("image/png", "aGVsbG8="))
    );
    assert_eq!(
        restored.content_str(),
        Some(prompt),
        "the restored turn must still be the original question"
    );
}
