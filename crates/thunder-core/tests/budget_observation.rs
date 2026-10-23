//! Phase C observation tests: the run-level budget is accounted for, the single
//! near-limit notice reaches the event sink as `AgentEvent::Custom`, and nothing is
//! enforced yet (phase D adds the wrap-up turn).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, AgentStats, LlmTurnResponse, LoopBudget, ReActAgentLoop, ScriptedMockDriver,
    StallConfig, ToolCallRequest,
};
use thunder_protocol::{
    AgentEvent, EventSink, ExecutionContext, ThunderEvent, ToolDescriptor, ToolExecutionError,
    ToolProvider,
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
    fn budget_notices(&self) -> Vec<serde_json::Value> {
        self.agent_events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Custom { kind, payload } if kind == "budget_notice" => {
                    Some(payload.clone())
                }
                _ => None,
            })
            .collect()
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

/// The loop bound comes from `budget.max_turns` alone (phase D merged the two knobs).
fn config(ws: &std::path::Path, budget: LoopBudget) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-budget".to_string(),
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

async fn run_with(budget: LoopBudget, turns: usize) -> (Arc<RecordingSink>, Vec<AgentEvent>) {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();

    let mut steps: Vec<LlmTurnResponse> = (0..turns)
        .map(|n| LlmTurnResponse::simple("again", vec![call(&format!("c{n}"), n)]))
        .collect();
    steps.push(LlmTurnResponse::simple("final answer", vec![]));

    let sink = Arc::new(RecordingSink::default());
    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(config(ws.path(), budget.clone()), registry, driver)
        .with_event_sink(sink.clone());
    agent.run("do work").await.expect("run completes");
    let events = sink.agent_events.lock().unwrap().clone();
    (sink, events)
}

#[tokio::test]
async fn near_limit_notice_is_emitted_once_as_a_custom_event() {
    let budget = LoopBudget {
        max_turns: 8,
        wall_clock: None,
        max_total_tokens: 0,
        warn_at: 0.5,
        ..LoopBudget::default()
    };
    let (sink, _) = run_with(budget, 6).await;

    let notices = sink.budget_notices();
    assert_eq!(
        notices.len(),
        1,
        "one notice per limit per run: {notices:?}"
    );
    assert_eq!(notices[0]["reason"], "near_limit");
    assert!(
        notices[0]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("max_turns"),
        "notice must name the limit closest to exhaustion: {notices:?}"
    );
}

#[tokio::test]
async fn budget_exhaustion_stops_the_run_and_wraps_up() {
    let budget = LoopBudget {
        max_turns: 2,
        wall_clock: None,
        max_total_tokens: 0,
        warn_at: 0.9,
        ..LoopBudget::default()
    };
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();

    // One tool turn, then the ceiling check fires on turn two and the next scripted
    // step is consumed by the wrap-up call.
    let mut steps: Vec<LlmTurnResponse> =
        vec![LlmTurnResponse::simple("again", vec![call("c0", 0)])];
    steps.push(LlmTurnResponse::simple("summary", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(config(ws.path(), budget), registry, driver);
    let outcome = agent
        .run("keep going")
        .await
        .expect("wrap-up is not an error");

    assert_eq!(
        outcome.finish_reason,
        thunder_core::FinishReason::MaxTurnsExceeded,
        "a turn ceiling is reported as such, not as a token budget"
    );
    assert_eq!(outcome.turns_executed, 2, "stops exactly at the budget");
    assert!(
        !outcome.final_response.is_empty(),
        "still reports a summary"
    );
}

#[tokio::test]
async fn quiet_budget_emits_nothing() {
    let budget = LoopBudget {
        wall_clock: Some(Duration::from_secs(600)),
        max_total_tokens: 0,
        warn_at: 0.75,
        ..LoopBudget::default()
    };
    let (sink, events) = run_with(budget, 3).await;
    assert!(sink.budget_notices().is_empty());
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::LoopComplete { .. })));
}

#[tokio::test]
async fn config_section_projects_onto_the_budget() {
    let section = thunder_core::AgentSection {
        max_turns: 42,
        wall_clock_secs: 90,
        max_total_tokens: 1234,
        max_tool_calls_per_turn: 3,
        warn_at: 0.6,
        max_context_resets: 1,
        max_restarts: 0,
        stall_mutation_free_turns: 5,
        stall_no_novel_turns: 4,
        ..thunder_core::AgentSection::default()
    };
    let budget = section.to_budget();
    assert_eq!(budget.max_turns, 42);
    assert_eq!(budget.wall_clock, Some(Duration::from_secs(90)));
    assert_eq!(budget.max_total_tokens, 1234);
    assert_eq!(budget.max_tool_calls_per_turn, 3);
    assert!((budget.warn_at - 0.6).abs() < f32::EPSILON);
    assert_eq!(budget.max_context_resets, 1);
    assert_eq!(budget.max_restarts, 0);

    // `0` means "unlimited" on the wire, `None` in the budget.
    let unlimited = thunder_core::AgentSection {
        wall_clock_secs: 0,
        ..thunder_core::AgentSection::default()
    };
    assert_eq!(unlimited.to_budget().wall_clock, None);
}

#[tokio::test]
async fn phase_c_defaults_stay_inert_for_existing_workspaces() {
    // A config file written before this change has no budget keys; the defaults must
    // reproduce the historical behaviour (16 turns, no enforcement).
    let parsed = thunder_core::ThunderConfig::parse_toml(
        r#"
[agent]
model = "gpt-4o"
api_base = "https://api.openai.com/v1"
max_turns = 16
max_context_tokens = 128000
"#,
    )
    .expect("legacy config must still parse");
    let budget = parsed.agent.to_budget();
    assert_eq!(budget.max_turns, 16);
    assert_eq!(budget.max_restarts, 0);
    assert!(budget.wall_clock.is_some(), "wall clock defaults to on");
    assert!(budget.warn_at > 0.0 && budget.warn_at < 1.0);
}

#[tokio::test]
async fn stats_survive_a_run_that_ends_normally() {
    let budget = LoopBudget {
        ..LoopBudget::default()
    };
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(EchoProvider))
        .await
        .unwrap();
    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("go", vec![call("a", 1)]),
        LlmTurnResponse::simple("done", vec![]),
    ]));
    let mut agent = ReActAgentLoop::new(config(ws.path(), budget.clone()), registry, driver);
    let outcome = agent.run("one call").await.expect("run completes");
    let AgentStats {
        total_turns,
        total_tool_executions,
        ..
    } = outcome.stats;
    assert_eq!(total_turns, 2);
    assert_eq!(total_tool_executions, 1);
}
