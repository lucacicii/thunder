use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, LlmTurnResponse, MiddlewarePipeline, ReActAgentLoop, ScriptedMockDriver,
    ToolCallRequest, ToolMiddleware,
};
use thunder_plan::{PlanModeMiddleware, PlanToolProvider};
use thunder_protocol::{ExecutionContext, ToolProvider};
use thunder_registry::ToolRegistry;

#[tokio::test]
async fn plan_middleware_blocks_writes_and_mutating_commands_but_allows_reads_and_plan_tools() {
    let ws = tempdir().unwrap();
    let ctx = ExecutionContext::new("sess-test", ws.path(), Duration::from_secs(5));
    let mw = PlanModeMiddleware;

    // Mutating tools blocked
    let r1 = mw
        .before_call(
            "core__write_file",
            &json!({"path": "a.txt", "content": "foo"}),
            &ctx,
        )
        .await;
    assert!(matches!(r1, thunder_core::MiddlewareVerdict::Reject { .. }));

    let r2 = mw
        .before_call(
            "core__edit_block",
            &json!({"path": "a.txt", "old_text": "foo", "new_text": "bar"}),
            &ctx,
        )
        .await;
    assert!(matches!(r2, thunder_core::MiddlewareVerdict::Reject { .. }));

    // Mutating bash commands blocked
    let r3 = mw
        .before_call("core__bash", &json!({"command": "git reset --hard"}), &ctx)
        .await;
    assert!(matches!(r3, thunder_core::MiddlewareVerdict::Reject { .. }));

    let r4 = mw
        .before_call("core__bash", &json!({"command": "echo hi > out.txt"}), &ctx)
        .await;
    assert!(matches!(r4, thunder_core::MiddlewareVerdict::Reject { .. }));

    // Read tools allowed
    let r5 = mw
        .before_call("core__read_file", &json!({"path": "a.txt"}), &ctx)
        .await;
    assert_eq!(r5, thunder_core::MiddlewareVerdict::Allow);

    let r6 = mw
        .before_call(
            "core__bash",
            &json!({"command": "git status --short"}),
            &ctx,
        )
        .await;
    assert_eq!(r6, thunder_core::MiddlewareVerdict::Allow);

    // Plan tools explicitly exempted
    let r7 = mw
        .before_call(
            "core__write_plan",
            &json!({"content": "# Implementation Plan"}),
            &ctx,
        )
        .await;
    assert_eq!(r7, thunder_core::MiddlewareVerdict::Allow);

    let r8 = mw
        .before_call(
            "core__exit_plan_mode",
            &json!({"explanation": "Plan ready"}),
            &ctx,
        )
        .await;
    assert_eq!(r8, thunder_core::MiddlewareVerdict::Allow);
}

#[tokio::test]
async fn plan_provider_writes_plan_and_triggers_mode_flip_callback() {
    let ws = tempdir().unwrap();
    let ctx = ExecutionContext::new("sess-plan-1", ws.path(), Duration::from_secs(5));
    let flipped = Arc::new(AtomicBool::new(false));
    let flipped_clone = Arc::clone(&flipped);

    let provider = PlanToolProvider::new().with_flip_callback(Arc::new(move |_sess, _exp| {
        flipped_clone.store(true, Ordering::SeqCst);
    }));

    // 1. exit_plan_mode fails if no plan was written
    let exit_err = provider
        .call_tool("core__exit_plan_mode", json!({}), &ctx)
        .await
        .expect_err("exit must fail before write");
    assert!(exit_err.to_string().contains("Call write_plan first"));

    // 2. write_plan writes to .thunder/plans/sess-plan-1.md
    let write_ok = provider
        .call_tool(
            "core__write_plan",
            json!({"content": "# Step 1: Refactor\n# Step 2: Test"}),
            &ctx,
        )
        .await
        .expect("write plan succeeds");
    assert!(write_ok.contains("Plan successfully saved"));

    let plan_on_disk = ws.path().join(".thunder/plans/sess-plan-1.md");
    assert!(plan_on_disk.exists());
    let saved_text = tokio::fs::read_to_string(&plan_on_disk).await.unwrap();
    assert_eq!(saved_text, "# Step 1: Refactor\n# Step 2: Test");

    // 3. exit_plan_mode now succeeds and triggers mode flip callback
    let exit_ok = provider
        .call_tool(
            "core__exit_plan_mode",
            json!({"explanation": "Reviewed"}),
            &ctx,
        )
        .await
        .expect("exit plan succeeds");
    assert!(exit_ok.contains("Plan mode exited successfully"));
    assert!(flipped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn e2e_agent_loop_operates_under_plan_mode_and_exits_cleanly() {
    let ws = tempdir().unwrap();
    let flipped = Arc::new(AtomicBool::new(false));
    let flipped_clone = Arc::clone(&flipped);

    let registry = ToolRegistry::new();
    let plan_provider = PlanToolProvider::shared(Some(Arc::new(move |_sess, _exp| {
        flipped_clone.store(true, Ordering::SeqCst);
    })));
    registry.register_provider(plan_provider).await.unwrap();

    let mw = MiddlewarePipeline::new().with_middleware(Arc::new(PlanModeMiddleware));

    // Turn 1: Model tries to write a code file (blocked by PlanModeMiddleware).
    // Turn 2: Model calls `core__write_plan` and `core__exit_plan_mode` (allowed!).
    // Turn 3: Final confirmation.
    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Attempting to write code directly without plan",
            vec![ToolCallRequest {
                id: "c1".into(),
                name: "core__write_file".into(),
                arguments: json!({"path": "src/lib.rs", "content": "fn foo() {}"}),
            }],
        ),
        LlmTurnResponse::simple(
            "Now writing plan and exiting plan mode",
            vec![
                ToolCallRequest {
                    id: "c2".into(),
                    name: "core__write_plan".into(),
                    arguments: json!({"content": "# Architecture Plan\n1. Design\n2. Implement"}),
                },
                ToolCallRequest {
                    id: "c3".into(),
                    name: "core__exit_plan_mode".into(),
                    arguments: json!({"explanation": "Plan complete"}),
                },
            ],
        ),
        LlmTurnResponse::simple("Plan approved and mode flip verified.", vec![]),
    ]));

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-plan-e2e".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        ..Default::default()
    };

    let mut agent =
        ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone()).with_middleware(mw);

    let outcome = agent.run("Plan feature").await.expect("run succeeds");
    assert_eq!(outcome.turns_executed, 3);
    assert_eq!(
        outcome.final_response,
        "Plan approved and mode flip verified."
    );
    assert!(
        flipped.load(Ordering::SeqCst),
        "exit_plan_mode must trigger flip callback"
    );

    // Verify turn 1 was blocked with 403
    let turn2_msgs = &mock_driver.recorded_messages()[1];
    let blocked_notice = turn2_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("[Middleware:plan-mode] Plan mode refuses tool 'core__write_file'"))
            .unwrap_or(false)
    });
    assert!(
        blocked_notice,
        "Turn 1 code write must be blocked by PlanModeMiddleware"
    );
}
