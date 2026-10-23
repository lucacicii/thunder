//! Phase D1: hitting a ceiling is no longer an error.
//!
//! The loop gives the model one final, tool-free turn so the user gets a summary, and
//! `wire_finish_reason()` keeps the client contract from
//! `docs/agent-loop-budget-and-recovery.md` §3.4: an answer means `done`, only a run
//! with nothing to show reports why it stopped.

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
    AgentEvent, ChatMessage, EventSink, ExecutionContext, ThunderEvent, ToolDescriptor,
    ToolExecutionError, ToolProvider,
};
use thunder_registry::ToolRegistry;

#[derive(Default)]
struct RecordingSink {
    agent_events: Mutex<Vec<AgentEvent>>,
}

impl EventSink for RecordingSink {
    fn on_event(&self, _event: ThunderEvent) {}

    fn on_agent_event(&self, event: AgentEvent) {
        self.agent_events.lock().unwrap().push(event);
    }
}

impl RecordingSink {
    fn notices(&self, reason: &str) -> Vec<serde_json::Value> {
        self.agent_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Custom { kind, payload }
                    if kind == "budget_notice" && payload["reason"] == reason =>
                {
                    Some(payload.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn loop_complete(&self) -> Option<(FinishReason, Option<String>)> {
        self.agent_events
            .lock()
            .unwrap()
            .iter()
            .find_map(|e| match e {
                AgentEvent::LoopComplete {
                    finish_reason,
                    final_content,
                    ..
                } => Some((*finish_reason, final_content.clone())),
                _ => None,
            })
    }
}

struct EchoProvider;

#[async_trait]
impl ToolProvider for EchoProvider {
    fn provider_id(&self) -> &str {
        "echo"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_mcp(
            "echo",
            "echo",
            "echo",
            json!({ "type": "object" }),
        )])
    }

    async fn call_tool(
        &self,
        _name: &str,
        arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        Ok(format!("echo:{arguments}"))
    }
}

fn config(ws: &std::path::Path, budget: LoopBudget) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-wrapup".to_string(),
        workspace_cwd: ws.to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: thunder_core::ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(5),
        cache_warm_settings: None,
        budget,
        stall: StallConfig::default(),
    }
}

fn call(id: &str, n: usize) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        name: "mcp__echo__echo".to_string(),
        arguments: json!({ "n": n }),
    }
}

/// A turn that reports real token usage, so token budgets can actually trip.
fn heavy_token_turn(content: &str, tool_calls: Vec<ToolCallRequest>) -> LlmTurnResponse {
    LlmTurnResponse {
        content: content.to_string(),
        reasoning: None,
        tool_calls,
        prompt_tokens: 5_000,
        completion_tokens: 100,
        cached_tokens: 0,
    }
}

/// Runs an explicit list of tool turns followed by wrap-up steps.
async fn run_hitting_ceiling_with(
    tool_steps: Vec<Result<LlmTurnResponse, String>>,
    wrap_up_steps: Vec<Result<LlmTurnResponse, String>>,
    budget: LoopBudget,
) -> (
    thunder_core::AgentLoopOutcome,
    Arc<RecordingSink>,
    Arc<ScriptedMockDriver>,
) {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();

    let mut steps = tool_steps;
    steps.extend(wrap_up_steps);

    let sink = Arc::new(RecordingSink::default());
    let driver = Arc::new(ScriptedMockDriver::with_results(steps));
    let mut agent = ReActAgentLoop::new(config(ws.path(), budget), registry, driver.clone())
        .with_event_sink(sink.clone());
    let outcome = agent
        .run("do a big job")
        .await
        .expect("wrap-up is not an error");
    (outcome, sink, driver)
}

/// Builds a registry plus `tool_turns` tool-calling steps followed by
/// `wrap_up_steps` (one per expected wrap-up call).
async fn run_hitting_ceiling(
    tool_turns: usize,
    wrap_up_steps: Vec<Result<LlmTurnResponse, String>>,
    budget: LoopBudget,
) -> (
    thunder_core::AgentLoopOutcome,
    Arc<RecordingSink>,
    Arc<ScriptedMockDriver>,
) {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();

    let mut steps: Vec<Result<LlmTurnResponse, String>> = (0..tool_turns)
        .map(|n| {
            Ok(LlmTurnResponse::simple(
                "working",
                vec![call(&format!("c{n}"), n)],
            ))
        })
        .collect();
    steps.extend(wrap_up_steps);

    let sink = Arc::new(RecordingSink::default());
    let driver = Arc::new(ScriptedMockDriver::with_results(steps));
    let mut agent = ReActAgentLoop::new(config(ws.path(), budget), registry, driver.clone())
        .with_event_sink(sink.clone());
    let outcome = agent
        .run("do a big job")
        .await
        .expect("wrap-up is not an error");
    (outcome, sink, driver)
}

#[tokio::test]
async fn hitting_the_turn_ceiling_returns_a_summary_instead_of_an_error() {
    let (outcome, sink, driver) = run_hitting_ceiling(
        1,
        vec![Ok(LlmTurnResponse::simple(
            "Done: wired the parser. Next: add tests.",
            vec![],
        ))],
        LoopBudget {
            max_turns: 2,
            wall_clock: None,
            ..LoopBudget::default()
        },
    )
    .await;

    assert_eq!(outcome.finish_reason, FinishReason::MaxTurnsExceeded);
    assert!(outcome.final_response.contains("wired the parser"));
    assert!(outcome.stopped_early());
    assert_eq!(
        outcome.wire_finish_reason(),
        "done",
        "an answer exists, so clients must not render a failure card"
    );

    // The wrap-up call must run with no tools at all, even though earlier turns had some.
    let recorded = driver.recorded_tool_prefixes();
    assert!(
        recorded.iter().any(|p| p != "[]"),
        "earlier turns expose tools"
    );
    assert_eq!(
        recorded.last().map(String::as_str),
        Some("[]"),
        "wrap-up turn must not expose any tool"
    );

    let (reason, content) = sink.loop_complete().expect("LoopComplete emitted");
    assert_eq!(reason, FinishReason::MaxTurnsExceeded);
    assert!(content.unwrap_or_default().contains("wired the parser"));
    assert_eq!(sink.notices("max_turns_exceeded").len(), 1);
}

#[tokio::test]
async fn wrap_up_prompt_forbids_tool_calls_and_asks_for_a_summary() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();
    let driver = Arc::new(ScriptedMockDriver::with_results(vec![
        Ok(LlmTurnResponse::simple("working", vec![call("c0", 0)])),
        Ok(LlmTurnResponse::simple("summary", vec![])),
    ]));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            LoopBudget {
                max_turns: 1,
                wall_clock: None,
                ..LoopBudget::default()
            },
        ),
        registry,
        driver.clone(),
    );
    agent.run("go").await.expect("run completes");

    let requests = driver.recorded_messages();
    let wrap_up_request = requests.last().expect("wrap-up request recorded");
    let notice = wrap_up_request
        .iter()
        .filter_map(ChatMessage::content_str)
        .find(|c| c.contains("<budget_notice>"))
        .expect("wrap-up prompt must be injected");
    assert!(notice.contains("Do not call any more tools"));
    assert!(notice.contains("what is done"));
}

#[tokio::test]
async fn budget_exhaustion_also_wraps_up() {
    // Turn ceiling disabled; the token budget is what stops this run.
    let (outcome, sink, _) = run_hitting_ceiling_with(
        vec![Ok(heavy_token_turn("working", vec![call("c0", 0)]))],
        vec![Ok(LlmTurnResponse::simple("Budget summary.", vec![]))],
        LoopBudget {
            max_turns: 0,
            wall_clock: None,
            max_total_tokens: 1,
            warn_at: 0.9,
            ..LoopBudget::default()
        },
    )
    .await;

    assert_eq!(outcome.finish_reason, FinishReason::BudgetExceeded);
    assert!(outcome.final_response.contains("Budget summary"));
    assert_eq!(sink.notices("budget_exceeded").len(), 1);
    assert_eq!(outcome.turns_executed, 2, "must stop right at the ceiling");
}

#[tokio::test]
async fn a_failed_wrap_up_falls_back_to_the_last_assistant_text() {
    let (outcome, _sink, _) = run_hitting_ceiling(
        1,
        vec![Err("network down".to_string())],
        LoopBudget {
            max_turns: 2,
            wall_clock: None,
            ..LoopBudget::default()
        },
    )
    .await;

    assert_eq!(outcome.finish_reason, FinishReason::MaxTurnsExceeded);
    assert_eq!(
        outcome.final_response, "working",
        "the last assistant text becomes the answer"
    );
    assert_eq!(outcome.wire_finish_reason(), "done");
}

#[tokio::test]
async fn a_run_with_no_content_reports_the_real_reason_on_the_wire() {
    let (outcome, _sink, _) = run_hitting_ceiling(
        1,
        vec![Ok(LlmTurnResponse::simple("", vec![]))],
        LoopBudget {
            max_turns: 2,
            wall_clock: None,
            ..LoopBudget::default()
        },
    )
    .await;

    // The first wrap-up attempt returned empty content and the retry step is unused,
    // so the loop still ends with a placeholder and a non-`done` wire reason.
    assert_eq!(outcome.finish_reason, FinishReason::MaxTurnsExceeded);
    assert_eq!(
        outcome.wire_finish_reason(),
        "max_turns_exceeded",
        "no answer at all must surface the reason to the client"
    );
    assert!(!outcome.stopped_early());
}
