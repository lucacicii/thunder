//! Phase D2: stall and cycle detection now stop the run.
//!
//! Both paths end in a wrap-up turn, so the user still gets a summary. The tests below
//! also pin down the two shapes that must **not** be touched: a run that keeps
//! producing new content, and an alternating pattern whose arguments evolve.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, FinishReason, LlmTurnResponse, LoopBudget, ReActAgentLoop, ScriptedMockDriver,
    StallConfig, ToolCallRequest,
};
use thunder_protocol::{
    ExecutionContext, ProgressKind, ToolDescriptor, ToolExecutionError, ToolProvider,
};
use thunder_registry::ToolRegistry;

struct Provider {
    kind: ProgressKind,
}

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
        .with_progress(self.kind)])
    }

    async fn call_tool(
        &self,
        _name: &str,
        arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        // Echo the arguments so distinct arguments produce distinct output.
        Ok(format!("out:{arguments}"))
    }
}

fn config(ws: &std::path::Path, max_turns: usize, stall: StallConfig) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-enforce".to_string(),
        workspace_cwd: ws.to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: thunder_core::ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(5),
        cache_warm_settings: None,
        budget: LoopBudget {
            max_turns,
            wall_clock: None,
            max_total_tokens: 0,
            // Recovery is covered by `recovery.rs`; these tests isolate enforcement.
            max_context_resets: 0,
            ..LoopBudget::default()
        },
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

#[tokio::test]
async fn a_repeating_cycle_aborts_and_wraps_up() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(Provider {
            kind: ProgressKind::Observing,
        }))
        .await
        .unwrap();

    // A-B-A-B-… with identical arguments: the cycle guard aborts on the eighth action,
    // which is the eighth scripted step; the ninth is the wrap-up.
    let mut steps: Vec<LlmTurnResponse> = Vec::new();
    for i in 0..8 {
        steps.push(LlmTurnResponse::simple(
            "cycling",
            vec![call(&format!("c{i}"), i % 2)],
        ));
    }
    steps.push(LlmTurnResponse::simple("CYCLE SUMMARY", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(ws.path(), 40, StallConfig::default()),
        registry,
        driver,
    );
    let outcome = agent
        .run("cycle forever")
        .await
        .expect("wrap-up is not an error");

    assert!(
        outcome.final_response.contains("CYCLE SUMMARY"),
        "expected the wrap-up summary, got {:?}",
        outcome.final_response
    );
    assert_eq!(outcome.turns_executed, 8);
    assert_eq!(outcome.finish_reason, FinishReason::BudgetExceeded);
}

#[tokio::test]
async fn evolving_arguments_are_never_treated_as_a_cycle() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(Provider {
            kind: ProgressKind::Mutating,
        }))
        .await
        .unwrap();

    // Same tool every turn with new content every turn: real progress, must finish.
    let mut steps: Vec<LlmTurnResponse> = (0..12)
        .map(|i| LlmTurnResponse::simple("editing", vec![call(&format!("c{i}"), i)]))
        .collect();
    steps.push(LlmTurnResponse::simple("all edits done", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(ws.path(), 40, StallConfig::default()),
        registry,
        driver,
    );
    let outcome = agent.run("edit many files").await.expect("run completes");
    assert_eq!(outcome.final_response, "all edits done");
    assert_eq!(outcome.finish_reason, FinishReason::Done);
    assert_eq!(outcome.stats.stall_events, 0);
}

#[tokio::test]
async fn mutation_free_streak_wraps_up_a_run_that_stopped_writing() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(Provider {
            kind: ProgressKind::Mutating,
        }))
        .await
        .unwrap();

    // One write, then nothing but reads whose output is identical (Neutral-like
    // novelty is not needed here: identical mutating args produce no progress).
    let mut steps: Vec<LlmTurnResponse> =
        vec![LlmTurnResponse::simple("write", vec![call("w0", 0)])];
    for i in 0..3 {
        // Same arguments: no new mutation digest.
        steps.push(LlmTurnResponse::simple(
            "read",
            vec![call(&format!("r{i}"), 0)],
        ));
    }
    steps.push(LlmTurnResponse::simple("STALL SUMMARY", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            40,
            StallConfig {
                mutation_free_turns: 3,
                no_novel_turns: 100,
            },
        ),
        registry,
        driver,
    );
    let outcome = agent
        .run("write then spin")
        .await
        .expect("wrap-up is not an error");

    assert!(
        outcome.final_response.contains("STALL SUMMARY"),
        "got {:?}",
        outcome.final_response
    );
    assert_eq!(outcome.finish_reason, FinishReason::BudgetExceeded);
    assert!(outcome.stats.stall_events >= 1);
    assert!(
        outcome.turns_executed < 6,
        "must stop before the whole script plays out, got {}",
        outcome.turns_executed
    );
}

#[tokio::test]
async fn read_only_investigation_is_never_mutation_stalled() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(Provider {
            kind: ProgressKind::Observing,
        }))
        .await
        .unwrap();

    // Twenty distinct reads with distinct output: a long but entirely normal
    // investigation. Mutation-free-streak must stay at zero because nothing was
    // ever written.
    let mut steps: Vec<LlmTurnResponse> = (0..20)
        .map(|i| LlmTurnResponse::simple("reading", vec![call(&format!("c{i}"), i)]))
        .collect();
    steps.push(LlmTurnResponse::simple("here is the analysis", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(steps));
    let mut agent = ReActAgentLoop::new(
        config(
            ws.path(),
            40,
            StallConfig {
                mutation_free_turns: 2,
                no_novel_turns: 6,
            },
        ),
        registry,
        driver,
    );
    let outcome = agent.run("investigate").await.expect("run completes");
    assert_eq!(outcome.final_response, "here is the analysis");
    assert_eq!(outcome.finish_reason, FinishReason::Done);
}
