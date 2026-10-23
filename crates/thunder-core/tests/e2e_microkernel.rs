use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;
use thunder_core::{
    AgentLoopConfig, ContextPruningConfig, LlmTurnResponse, ReActAgentLoop, ScriptedMockDriver,
    ToolCallRequest, CHECKPOINT_MESSAGE_NAME, META_CALL_TOOL, META_DISCOVER_TOOLS,
};
use thunder_executor::{McpServerConfig, McpToolProvider};
use thunder_protocol::ChatMessage;
use thunder_registry::{start_hot_reload_watcher, ToolRegistry};

fn find_or_build_thunder_std() -> PathBuf {
    let current_exe = std::env::current_exe().expect("current_exe");
    let target_dir = current_exe
        .parent()
        .and_then(|p| p.parent())
        .expect("target/debug directory");
    let bin = target_dir.join("thunder-std");
    if bin.exists() {
        return bin;
    }
    panic!(
        "thunder-std binary not found at {}; run `cargo test --workspace` so all binaries build",
        bin.display()
    );
}

#[tokio::test]
async fn e2e_self_evolving_agent_hot_loads_script_with_100_percent_kv_cache_shield() {
    let ws = tempdir().unwrap();
    let ws_path = ws.path().canonicalize().unwrap();
    let tools_dir = ws_path.join(".thunder").join("tools");
    tokio::fs::create_dir_all(&tools_dir).await.unwrap();

    // 1. Mount `thunder-std` Core MCP server over Stdio JSON-RPC
    let std_bin = find_or_build_thunder_std();
    let std_provider =
        McpToolProvider::connect("thunder-std", &McpServerConfig::new(std_bin).as_core())
            .await
            .expect("connect thunder-std");

    let registry = ToolRegistry::new();
    registry.register_provider(std_provider).await.unwrap();

    // 2. Start the 200ms debounced `.thunder/tools/` watcher
    let _watcher = start_hot_reload_watcher(registry.clone(), tools_dir.clone(), None)
        .await
        .expect("start watcher");

    let script_body = r#"#!/usr/bin/env bash
# @thunder
# name: git_squash_feature
# description: 自动压缩分支提交
# parameters:
#   type: object
#   properties:
#     base_branch:
#       type: string
#   required: [base_branch]
# @end
set -euo pipefail
input=$(cat)
echo -n "SQUASH_OK:${input}"
"#;

    let mock_driver_1 = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Writing custom script tool into .thunder/tools/",
            vec![ToolCallRequest {
                id: "call-1".to_string(),
                name: "core__write_file".to_string(),
                arguments: json!({
                    "path": ".thunder/tools/git_squash_feature.sh",
                    "content": script_body
                }),
            }],
        ),
        LlmTurnResponse::simple("Created script tool successfully.", vec![]),
    ]));

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-e2e".to_string(),
        workspace_cwd: ws_path.clone(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        pruning: ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(10),
        budget: thunder_core::LoopBudget {
            ..Default::default()
        },
        stall: thunder_core::StallConfig::default(),
        cache_warm_settings: None,
    };

    let mut agent = ReActAgentLoop::new(loop_cfg, registry.clone(), mock_driver_1.clone());
    let outcome1 = agent
        .run("Create a git squash tool")
        .await
        .expect("turn 1 succeeds");
    assert_eq!(outcome1.turns_executed, 2);

    // Wait for the background 200ms debounced watcher actor to detect the file and swap ArcSwap<CatalogSnapshot>
    let v0 = registry.current().version;
    let mut updated = false;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if registry.current().version > v0 {
            updated = true;
            break;
        }
    }
    assert!(updated, "Watcher must update CatalogSnapshot within 2s");

    let mock_driver_2 = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Now discovering and invoking the newly hot-loaded script via gateway",
            vec![
                ToolCallRequest {
                    id: "call-3".to_string(),
                    name: META_DISCOVER_TOOLS.to_string(),
                    arguments: json!({ "query": "git_squash_feature" }),
                },
                ToolCallRequest {
                    id: "call-4".to_string(),
                    name: META_CALL_TOOL.to_string(),
                    arguments: json!({
                        "name": "script__git_squash_feature",
                        "arguments": { "base_branch": "main" }
                    }),
                },
            ],
        ),
        LlmTurnResponse::simple("All feature commits have been squashed onto main.", vec![]),
    ]));

    // Continue the SAME agent session: KV-Cache Shield detects snapshot version bump (v0 -> v1)
    // and injects <system_notification>, while keeping top-level `tools` 100% byte-identical!
    agent.set_llm(mock_driver_2.clone());

    let outcome2 = agent
        .run("Now run git_squash_feature against main")
        .await
        .expect("turn 2 succeeds");
    assert_eq!(outcome2.turns_executed, 2);
    assert_eq!(
        outcome2.final_response,
        "All feature commits have been squashed onto main."
    );

    // Verify 100% KV-Cache prefix preservation across runs:
    let prefixes1 = mock_driver_1.recorded_tool_prefixes();
    let prefixes2 = mock_driver_2.recorded_tool_prefixes();
    assert_eq!(
        prefixes1[0], prefixes2[0],
        "Top-level tools must remain 100% static across runs even after dynamic scripts are mounted"
    );

    // Verify `<system_notification>[Registry Update]` was injected in session continuation!
    let run2_messages = &mock_driver_2.recorded_messages()[0];
    let has_registry_notif = run2_messages.iter().any(|m| {
        m.content_str()
            .map(|c| {
                c.contains("<system_notification>")
                    && c.contains("[Registry Update] 工具已挂载: `script__git_squash_feature`")
            })
            .unwrap_or(false)
    });
    assert!(
        has_registry_notif,
        "Run 2 must contain progressive disclosure <system_notification>"
    );
}

#[tokio::test]
async fn gateway_and_direct_calls_both_enforce_safety_middleware() {
    let ws = tempdir().unwrap();
    let std_bin = find_or_build_thunder_std();
    let std_provider =
        McpToolProvider::connect("thunder-std", &McpServerConfig::new(std_bin).as_core())
            .await
            .expect("connect thunder-std");

    let registry = ToolRegistry::new();
    registry.register_provider(std_provider).await.unwrap();

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Attempting dangerous command via gateway",
            vec![ToolCallRequest {
                id: "call-danger".to_string(),
                name: META_CALL_TOOL.to_string(),
                arguments: json!({
                    "name": "core__bash",
                    "arguments": { "command": "rm -rf /" }
                }),
            }],
        ),
        LlmTurnResponse::simple("Command was blocked by middleware as expected.", vec![]),
    ]));

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-mw".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        ..Default::default()
    };

    struct TestSafetyMiddleware;
    #[async_trait::async_trait]
    impl thunder_core::ToolMiddleware for TestSafetyMiddleware {
        fn id(&self) -> &str {
            "safety-guard"
        }
        async fn before_call(
            &self,
            _tool: &str,
            arguments: &serde_json::Value,
            _ctx: &thunder_protocol::ExecutionContext,
        ) -> thunder_core::MiddlewareVerdict {
            if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("rm -rf /") {
                    return thunder_core::MiddlewareVerdict::Reject {
                        reason: "Blocked destructive rm -rf /".to_string(),
                    };
                }
            }
            thunder_core::MiddlewareVerdict::Allow
        }
    }

    let mw =
        thunder_core::MiddlewarePipeline::new().with_middleware(Arc::new(TestSafetyMiddleware));

    let mut agent =
        ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone()).with_middleware(mw);
    let outcome = agent.run("test safety").await.unwrap();
    assert_eq!(outcome.turns_executed, 2);

    let turn2_messages = &mock_driver.recorded_messages()[1];
    let blocked = turn2_messages.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("[Middleware:safety-guard]") && c.contains("rm -rf /"))
            .unwrap_or(false)
    });
    assert!(
        blocked,
        "Gateway call must not bypass SafetyGuardMiddleware"
    );
}

#[tokio::test]
async fn checkpoint_compaction_and_overflow_self_healing_preserve_tool_pairing() {
    let ws = tempdir().unwrap();
    let registry = ToolRegistry::new();

    // Simulate a context overflow on the first attempt, followed by a successful retry after checkpoint compaction.
    let mock_driver = Arc::new(ScriptedMockDriver::with_results(vec![
        Err("This model's maximum context length is 4096 tokens. However, your messages resulted in 5000 tokens.".to_string()),
        Ok(LlmTurnResponse::simple("Compacted and recovered!", vec![])),
    ]));

    let pruning = ContextPruningConfig {
        max_context_tokens: 128_000,
        keep_recent_tokens: 30,
        reserve_tokens: 20,
        ..ContextPruningConfig::default()
    };

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-compact".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        pruning,
        ..Default::default()
    };

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone())
        .with_summarizer_client(mock_driver.clone());

    let outcome = agent
        .run("Please summarize the earlier work and continue")
        .await
        .expect("overflow auto-heals");

    assert_eq!(outcome.final_response, "Compacted and recovered!");
    assert_eq!(outcome.compactions_triggered, 1);

    // Also verify direct checkpoint compaction on a multi-turn buffer inserts `context_checkpoint`.
    let mut ctx = thunder_core::ContextBuffer::with_system_prompt("sys");
    ctx.push(ChatMessage::user(
        "first question with some context ".repeat(20),
    ));
    ctx.push(ChatMessage::assistant_text("first answer ".repeat(20)));
    ctx.push(ChatMessage::user("second question"));

    let pruner = thunder_core::ContextPruner::new(ContextPruningConfig {
        max_context_tokens: 100,
        reserve_tokens: 20,
        keep_recent_tokens: 15,
        summarizer_model: Some("gpt-4o".into()),
        summarizer_max_tokens: 512,
    });
    let summarizer = thunder_core::Summarizer::new(
        mock_driver,
        "gpt-4o",
        &ContextPruningConfig::default(),
        Arc::new(tokio::sync::Semaphore::new(1)),
    );
    let prune_res = pruner
        .prune(
            &mut ctx,
            Some(&summarizer),
            &tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(prune_res.pruned);
    assert!(prune_res.checkpoint);
    let has_checkpoint = ctx.get_messages().iter().any(|m| {
        matches!(m, ChatMessage::System { name, .. } if name.as_deref() == Some(CHECKPOINT_MESSAGE_NAME))
    });
    assert!(has_checkpoint);
}

#[tokio::test]
async fn steer_queues_and_custom_middleware_work_end_to_end() {
    let ws = tempdir().unwrap();

    let std_bin = find_or_build_thunder_std();
    let std_provider =
        McpToolProvider::connect("thunder-std", &McpServerConfig::new(std_bin).as_core())
            .await
            .expect("connect thunder-std");

    let registry = ToolRegistry::new();
    registry.register_provider(std_provider).await.unwrap();

    let steer = thunder_core::SteerQueues::new_shared();
    steer
        .follow_up
        .enqueue(ChatMessage::user("Follow-up task: run git status"));

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("Initial answer.", vec![]),
        LlmTurnResponse::simple(
            "Testing middleware checks",
            vec![
                ToolCallRequest {
                    id: "c1".into(),
                    name: "core__bash".into(),
                    arguments: json!({ "command": "echo safe_ok" }),
                },
                ToolCallRequest {
                    id: "c2".into(),
                    name: "core__bash".into(),
                    arguments: json!({ "command": "git reset --hard" }),
                },
            ],
        ),
        LlmTurnResponse::simple("Done after follow-up.", vec![]),
    ]));

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-steer-mw".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        ..Default::default()
    };

    struct TestSafetyMiddleware;
    #[async_trait::async_trait]
    impl thunder_core::ToolMiddleware for TestSafetyMiddleware {
        fn id(&self) -> &str {
            "test-safety"
        }
        async fn before_call(
            &self,
            _tool: &str,
            arguments: &serde_json::Value,
            _ctx: &thunder_protocol::ExecutionContext,
        ) -> thunder_core::MiddlewareVerdict {
            if let Some(cmd) = arguments.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("git reset") {
                    return thunder_core::MiddlewareVerdict::Reject {
                        reason: "Blocked git reset".to_string(),
                    };
                }
            }
            thunder_core::MiddlewareVerdict::Allow
        }
    }

    let mw =
        thunder_core::MiddlewarePipeline::new().with_middleware(Arc::new(TestSafetyMiddleware));

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone())
        .with_steer_queues(steer)
        .with_middleware(mw);

    let outcome = agent.run("Start task").await.expect("run succeeds");
    assert_eq!(outcome.turns_executed, 3);
    assert_eq!(outcome.final_response, "Done after follow-up.");

    let turn3_msgs = &mock_driver.recorded_messages()[2];
    let allowed_echo = turn3_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("safe_ok"))
            .unwrap_or(false)
    });
    let rejected_reset = turn3_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("[Middleware:test-safety] Blocked git reset"))
            .unwrap_or(false)
    });
    assert!(allowed_echo, "Custom middleware must allow safe_ok");
    assert!(
        rejected_reset,
        "Custom middleware must reject mutating git reset"
    );
}
