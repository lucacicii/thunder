//! Phase B observation-mode tests: the progress tracker must be wired into a real
//! run — plugin-declared [`ProgressKind`] resolved from the frozen turn snapshot,
//! digests recorded per tool call, and counters surfaced through `AgentStats`.
//!
//! Nothing here asserts control flow: phase B never aborts on a stall signal.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, LlmTurnResponse, ReActAgentLoop, ScriptedMockDriver, ToolCallRequest,
};
use thunder_protocol::{
    ChatMessage, ExecutionContext, ProgressKind, ToolDescriptor, ToolExecutionError, ToolProvider,
};
use thunder_registry::ToolRegistry;

struct KindedProvider {
    kind: ProgressKind,
    /// Output counter, so each call can return distinct text when asked.
    calls: std::sync::Mutex<usize>,
    distinct_output: bool,
}

#[async_trait]
impl ToolProvider for KindedProvider {
    fn provider_id(&self) -> &str {
        "kinded"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![ToolDescriptor::new_mcp(
            "kinded",
            "mut",
            "writes",
            json!({ "type": "object" }),
        )
        .with_progress(self.kind)])
    }

    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Value,
        _ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        let mut n = self.calls.lock().unwrap();
        *n += 1;
        if self.distinct_output {
            Ok(format!("output {}", n))
        } else {
            Ok("constant output".to_string())
        }
    }
}

fn config(ws: &std::path::Path, max_turns: usize) -> AgentLoopConfig {
    AgentLoopConfig {
        session_id: "sess-progress".to_string(),
        workspace_cwd: ws.to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: thunder_core::ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(5),
        budget: thunder_core::LoopBudget {
            max_turns,
            max_context_resets: 0,
            ..Default::default()
        },
        stall: thunder_core::StallConfig::default(),
        cache_warm_settings: None,
    }
}

fn call(id: &str, n: u32) -> ToolCallRequest {
    ToolCallRequest {
        id: id.to_string(),
        name: "mcp__kinded__mut".to_string(),
        arguments: json!({ "n": n }),
    }
}

#[tokio::test]
async fn mutating_tool_counts_one_mutation_per_distinct_argument_set() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(KindedProvider {
            kind: ProgressKind::Mutating,
            calls: std::sync::Mutex::new(0),
            distinct_output: true,
        }))
        .await
        .unwrap();

    // Turn 1 writes twice with different content, turn 2 rewrites the same content.
    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("writing", vec![call("a", 1), call("b", 2)]),
        LlmTurnResponse::simple("rewriting", vec![call("c", 1)]),
        LlmTurnResponse::simple("done", vec![]),
    ]));

    let mut agent = ReActAgentLoop::new(config(ws.path(), 8), registry, driver);
    let outcome = agent.run("write things").await.expect("run completes");

    assert_eq!(
        outcome.stats.mutations, 2,
        "two distinct content digests, the third write repeats the first"
    );
    assert_eq!(
        outcome.stats.novel_observations, 2,
        "a novel write is novel information: it must reset the no-novelty streak"
    );
}

#[tokio::test]
async fn observing_tool_with_constant_output_is_not_novel_after_the_first_call() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(KindedProvider {
            kind: ProgressKind::Observing,
            calls: std::sync::Mutex::new(0),
            distinct_output: false,
        }))
        .await
        .unwrap();

    // Same arguments, same output: the second read teaches the agent nothing.
    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("read 1", vec![call("a", 1)]),
        LlmTurnResponse::simple("read 2", vec![call("b", 1)]),
        LlmTurnResponse::simple("done", vec![]),
    ]));

    let mut agent = ReActAgentLoop::new(config(ws.path(), 8), registry, driver);
    let outcome = agent.run("read things").await.expect("run completes");

    assert_eq!(
        outcome.stats.novel_observations, 1,
        "identical read output twice is one piece of information"
    );
    assert_eq!(outcome.stats.mutations, 0);
}

#[tokio::test]
async fn a_no_progress_run_wraps_up_instead_of_burning_the_budget() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(KindedProvider {
            kind: ProgressKind::Neutral,
            calls: std::sync::Mutex::new(0),
            distinct_output: false,
        }))
        .await
        .unwrap();

    // A `bash`-shaped loop: every call has different arguments (so the loop guard's
    // repetition detector stays quiet) but prints the same thing (so the progress
    // tracker sees no new information). Seven fruitless turns reach the stall
    // threshold on the seventh, and the eighth scripted step is the wrap-up summary.
    let mut turns: Vec<LlmTurnResponse> = (0..7)
        .map(|n| LlmTurnResponse::simple("again", vec![call(&format!("c{n}"), n)]))
        .collect();
    turns.push(LlmTurnResponse::simple("SUMMARY: nothing new.", vec![]));

    let driver = Arc::new(ScriptedMockDriver::new(turns));
    let mut agent = ReActAgentLoop::new(config(ws.path(), 40), registry, driver);
    let outcome = agent
        .run("repeat forever")
        .await
        .expect("wrap-up is not an error");

    assert_eq!(
        outcome.finish_reason,
        thunder_core::FinishReason::BudgetExceeded
    );
    assert!(
        outcome.final_response.contains("SUMMARY"),
        "the wrap-up turn must supply the answer, got {:?}",
        outcome.final_response
    );
    assert_eq!(
        outcome.turns_executed, 7,
        "stops exactly when the stall fires"
    );
    assert_eq!(outcome.stats.stall_events, 1);
}

#[tokio::test]
async fn tool_results_stay_paired_while_observing_progress() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();
    registry
        .register_provider(Arc::new(KindedProvider {
            kind: ProgressKind::Mutating,
            calls: std::sync::Mutex::new(0),
            distinct_output: true,
        }))
        .await
        .unwrap();

    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("write", vec![call("a", 1), call("b", 2), call("c", 3)]),
        LlmTurnResponse::simple("done", vec![]),
    ]));
    let mut agent = ReActAgentLoop::new(config(ws.path(), 8), registry, driver);
    agent.run("write three").await.expect("run completes");

    let messages = agent.context().get_messages();
    let pending: Vec<&str> = messages
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => Some(calls.iter().map(|c| c.id.as_str()).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(pending, vec!["a", "b", "c"]);
}
