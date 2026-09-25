//! Real-mode (non-mock) integration tests for the orchestra client seam.
//!
//! Regression guard for the "green tests, broken production" trap: every unit
//! (and the synthesizer) must resolve its LLM client through the configured
//! `ClientFactory` when `use_mock == false`, and a missing factory must fail
//! fast at dispatch time instead of dying mid-run on `UnconfiguredLLMClient`.

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use thunder_agent_loop::{
    AgentConfig, ChatMessage, ChatRequestOptions, FinishReason, LLMClientTrait, LLMStreamChunk,
};
use thunder_orchestra::{ClientFactory, OrchestraConfig, Scheduler, Topology, UnitSpec};
use tokio_util::sync::CancellationToken;

/// Fake completion client that records every request into shared state and
/// immediately completes with the literal text "ok". Stands in for the
/// pi-bridge transport so the NON-mock code path is exercised hermetically.
struct RecordingClient {
    calls: Arc<AtomicUsize>,
    last_user_prompts: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl LLMClientTrait for RecordingClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let last_user = options
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User { content, .. } => Some(content.clone()),
                _ => None,
            })
            .unwrap_or_default();
        self.last_user_prompts.lock().unwrap().push(last_user);

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("ok".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(4),
                    completion_tokens: Some(2),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("thunder_orch_real_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[tokio::test]
async fn real_mode_runs_units_and_synthesizer_through_client_factory() {
    let root = temp_root("happy");
    let calls = Arc::new(AtomicUsize::new(0));
    let prompts: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));

    let calls_c = calls.clone();
    let prompts_c = prompts.clone();
    let factory: ClientFactory = Arc::new(move |_cfg: &AgentConfig| {
        Some(Arc::new(RecordingClient {
            calls: calls_c.clone(),
            last_user_prompts: prompts_c.clone(),
        }) as Arc<dyn LLMClientTrait>)
    });

    let base = AgentConfig::new("fake-model");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_client_factory(factory)
        .with_synthesizer_unit(UnitSpec::new(
            "synthesizer",
            "synthesizer",
            base.clone(),
        ))
        .with_unit(UnitSpec::new("planner", "planner", base.clone()))
        .with_unit(UnitSpec::new("reviewer", "reviewer", base.clone()));

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch("review the module", false, None)
        .await
        .expect("real-mode dispatch must succeed with a client factory");

    // Both units actually ran through the factory-backed client.
    assert_eq!(run.results.len(), 2);
    for (role, res) in &run.results {
        assert_eq!(res.finish_reason, FinishReason::Done, "role={role}");
        assert_eq!(res.final_content.as_deref(), Some("ok"), "role={role}");
    }

    // 2 units + 1 synthesizer all streamed through the injected client.
    assert!(
        calls.load(Ordering::SeqCst) >= 3,
        "factory client invocations = {}",
        calls.load(Ordering::SeqCst)
    );
    let seen = prompts.lock().unwrap().clone();
    assert!(
        seen.iter().any(|p| p.contains("Synthesis and Review Aggregator")),
        "synthesizer prompt not observed: {seen:?}"
    );

    // Synthesis came from the REAL LLM path (fake client text), not the
    // honest-concatenation fallback ("Unit Outputs ...").
    let syn = run.synthesis.expect("synthesis must be present");
    assert_eq!(syn, "ok");
    assert!(!syn.contains("Unit Outputs"));
}

#[tokio::test]
async fn real_mode_without_client_factory_fails_fast() {
    let root = temp_root("nofactory");
    let base = AgentConfig::new("m");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_unit(UnitSpec::new("planner", "planner", base.clone()))
        .with_unit(UnitSpec::new("reviewer", "reviewer", base));

    let scheduler = Scheduler::new(orchestra);
    let err = match scheduler.dispatch("hello", false, None).await {
        Err(e) => e,
        Ok(_) => panic!("real mode without a factory must fail fast"),
    };
    assert!(
        err.to_string().contains("client factory"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn real_mode_factory_returning_none_reports_unit_and_model() {
    let root = temp_root("noneclient");
    let base = AgentConfig::new("unknown-model");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_client_factory(Arc::new(|_cfg: &AgentConfig| None))
        .with_unit(UnitSpec::new("planner", "planner", base.clone()))
        .with_unit(UnitSpec::new("reviewer", "reviewer", base));

    let scheduler = Scheduler::new(orchestra);
    let err = match scheduler.dispatch("hello", false, None).await {
        Err(e) => e,
        Ok(_) => panic!("factory returning None must fail"),
    };
    let msg = err.to_string();
    assert!(msg.contains("returned no client"), "unexpected error: {msg}");
    assert!(msg.contains("planner"), "error should name the unit: {msg}");
    assert!(msg.contains("unknown-model"), "error should name the model: {msg}");
}

#[tokio::test]
async fn mock_mode_still_runs_without_factory() {
    let root = temp_root("mockregression");
    let base = AgentConfig::new("m");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_unit(UnitSpec::new("planner", "planner", base.clone()))
        .with_unit(UnitSpec::new("reviewer", "reviewer", base));

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch("ship it", true, None)
        .await
        .expect("mock dispatch needs no factory");
    assert_eq!(run.results.len(), 2);
}
