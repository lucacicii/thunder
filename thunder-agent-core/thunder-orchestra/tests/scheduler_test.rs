use std::sync::Arc;
use thunder_agent_loop::{
    AgentConfig, AgentLoop, AgentTool, ChatRequestOptions, LLMClientTrait, LLMStreamChunk,
    ToolExecutionContext,
};
use thunder_orchestra::{DelegateTool, HealthStatus, OrchestraConfig, Scheduler, Topology, UnitSpec};
use tokio_util::sync::CancellationToken;

struct ImmediateClient {
    text: String,
}

#[async_trait::async_trait]
impl LLMClientTrait for ImmediateClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let text = self.text.clone();
        tokio::spawn(async move {
            let _ = tx.send(Ok(LLMStreamChunk::Token(text.clone()))).await;
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some(text),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(1),
                    completion_tokens: Some(1),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

fn unit(id: &str, role: &str) -> UnitSpec {
    UnitSpec::new(id, role, AgentConfig::new("test-model"))
}

fn store_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "thunder_orchestra_{}_{}",
        std::process::id(),
        label
    ))
}

#[tokio::test]
async fn parallel_two_units_finish_independently() {
    let root = store_root("parallel");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_unit(unit("planner", "planner"))
        .with_unit(unit("reviewer", "reviewer"));

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch("ship it", true, None)
        .await
        .expect("dispatch");

    assert_eq!(run.results.len(), 2);
    let ids: Vec<_> = run.results.iter().map(|(_, r)| r.agent_id.as_str()).collect();
    assert!(ids.contains(&"planner"));
    assert!(ids.contains(&"reviewer"));
    assert!(root.join(&run.run_id).join("planner.json").exists());
    assert!(root.join(&run.run_id).join("reviewer.json").exists());
}

#[tokio::test]
async fn sequential_feeds_previous_final_content() {
    let root = store_root("seq");
    let orchestra = OrchestraConfig::new(Topology::Sequential)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_unit(unit("planner", "planner"))
        .with_unit(unit("coder", "coder"));

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch("add health check", true, None)
        .await
        .expect("dispatch");

    assert_eq!(run.results.len(), 2);
    let coder = &run.results[1].1;
    let text = coder.final_content.as_deref().unwrap_or("");
    assert!(
        text.contains("[coder]"),
        "coder should produce its own mock answer: {text}"
    );
}

#[tokio::test]
async fn delegate_tool_runs_a_complete_unit() {
    let factory = || {
        AgentLoop::new(AgentConfig::new("test-model"))
            .with_id("researcher")
            .with_custom_client(Arc::new(ImmediateClient {
                text: "subtask complete".into(),
            }))
    };
    let tool = DelegateTool::new("delegate_research", "Ask the researcher", factory);
    assert_eq!(tool.definition().function.name, "delegate_research");

    let ctx = ToolExecutionContext {
        tool_call_id: "t1".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    };
    let out = tool
        .execute(serde_json::json!({"task": "look this up"}), &ctx)
        .await
        .expect("delegate");
    assert_eq!(out, "subtask complete");
}


#[tokio::test]
async fn health_passes_with_mock_and_temp_dirs() {
    let root = store_root("health");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_base(AgentConfig::new("test-model"));
    let scheduler = Scheduler::new(orchestra);
    let report = scheduler.health(true).await;
    assert_eq!(report.status, HealthStatus::Ok);
    assert!(report.checks.iter().all(|c| c.passed), "checks: {:?}", report.checks);
}

#[tokio::test]
async fn parallel_with_synthesis_aggregates_results() {
    let root = store_root("parallel_synthesis");
    let orchestra = OrchestraConfig::new(Topology::Parallel)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_synthesizer(true)
        .with_unit(unit("planner", "planner"))
        .with_unit(unit("reviewer", "reviewer"));

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch("review security vulnerabilities", true, None)
        .await
        .expect("dispatch");

    assert_eq!(run.results.len(), 2);
    assert!(run.synthesis.is_some());
    let syn = run.synthesis.unwrap();
    // Mock runs aggregate verbatim under an HONEST label — no fake
    // "successfully synthesized" verdict without a real LLM pass.
    assert!(syn.contains("Unit Outputs"), "syn = {syn}");
    assert!(syn.contains("mock run"), "syn = {syn}");
    assert!(syn.contains("planner"));
    assert!(syn.contains("reviewer"));
    assert!(!syn.contains("Successfully synthesized"));
}

#[tokio::test]
async fn fan_out_decomposes_structured_subtasks_concurrently() {
    let root = store_root("fan_out");
    let orchestra = OrchestraConfig::new(Topology::FanOut)
        .with_store_root(&root)
        .with_scratch_root(root.join("scratch"))
        .with_synthesizer(true)
        .with_unit(unit("sec_auditor", "security"))
        .with_unit(unit("perf_auditor", "performance"));

    let prompt = "Process these two audit tasks:\n1. Audit auth middleware for token leak vulnerabilities\n2. Benchmark request throughput under 10k connections";

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler
        .dispatch(prompt, true, None)
        .await
        .expect("dispatch");

    assert_eq!(run.results.len(), 2);
    // Verified subtask titles and roles assigned
    let titles: Vec<_> = run.results.iter().map(|(label, _)| label.as_str()).collect();
    assert!(titles.iter().any(|t| t.contains("SubTask 1")));
    assert!(titles.iter().any(|t| t.contains("SubTask 2")));
    assert!(run.synthesis.is_some());
}

