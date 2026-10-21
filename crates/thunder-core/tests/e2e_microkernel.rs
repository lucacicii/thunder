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
    let tools_dir = ws.path().join(".thunder").join("tools");
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

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
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

    let loop_cfg = AgentLoopConfig {
        session_id: "sess-e2e".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        system_prompt: "You are Thunder Agent.".to_string(),
        model: "gpt-4o".to_string(),
        max_turns: 8,
        pruning: ContextPruningConfig::default(),
        tool_timeout: Duration::from_secs(10),
        cache_warm_settings: None,
        load_project_instructions: false,
    };

    let mut agent = ReActAgentLoop::new(loop_cfg, registry.clone(), mock_driver.clone());
    let outcome = agent
        .run("Create a git squash tool and run it against main")
        .await
        .expect("agent loop succeeds");

    assert_eq!(outcome.turns_executed, 3);
    assert_eq!(
        outcome.final_response,
        "All feature commits have been squashed onto main."
    );
    assert!(outcome.system_faults.is_empty());

    // Verify 100% KV-Cache prefix preservation:
    let prefixes = mock_driver.recorded_tool_prefixes();
    assert_eq!(prefixes.len(), 3);
    assert_eq!(
        prefixes[0], prefixes[1],
        "Top-level tools must not mutate on hot-reload"
    );
    assert_eq!(
        prefixes[1], prefixes[2],
        "Top-level tools must remain static across all turns"
    );

    // Verify `<system_notification>[Registry Update]` was injected before Turn 2!
    let turn2_messages = &mock_driver.recorded_messages()[1];
    let has_registry_notif = turn2_messages.iter().any(|m| {
        m.content_str()
            .map(|c| {
                c.contains("<system_notification>")
                    && c.contains("[Registry Update] 工具已挂载: `script__git_squash_feature`")
            })
            .unwrap_or(false)
    });
    assert!(
        has_registry_notif,
        "Turn 2 must contain progressive disclosure <system_notification>"
    );

    // Verify the script execution result reached Turn 3 context.
    let turn3_messages = &mock_driver.recorded_messages()[2];
    let has_script_output = turn3_messages.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains(r#"SQUASH_OK:{"base_branch":"main"}"#))
            .unwrap_or(false)
    });
    assert!(
        has_script_output,
        "Turn 3 context must contain the output of script__git_squash_feature"
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

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone());
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
async fn agents_md_instructions_and_steer_queues_and_plan_mode_work_end_to_end() {
    let ws = tempdir().unwrap();
    tokio::fs::write(ws.path().join("AGENTS.md"), "RULE_42: Always verify tests.")
        .await
        .unwrap();

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

    // Turn 1 finishes without tool calls, which triggers the queued `follow_up` message for Turn 2!
    // Turn 2 tries a read-only `git status` and a mutating `git reset --hard` under PlanModeMiddleware.
    // Turn 3 concludes.
    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple("Initial answer.", vec![]),
        LlmTurnResponse::simple(
            "Testing plan mode commands",
            vec![
                ToolCallRequest {
                    id: "c1".into(),
                    name: "core__bash".into(),
                    arguments: json!({ "command": "echo safe_read_ok" }),
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
        session_id: "sess-steer-plan".to_string(),
        workspace_cwd: ws.path().to_path_buf(),
        load_project_instructions: true,
        ..Default::default()
    };

    let mw = thunder_core::MiddlewarePipeline::new()
        .with_middleware(Arc::new(thunder_core::PlanModeMiddleware));

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, mock_driver.clone())
        .with_steer_queues(steer)
        .with_middleware(mw);

    let outcome = agent.run("Start task").await.expect("run succeeds");
    assert_eq!(outcome.turns_executed, 3);
    assert_eq!(outcome.final_response, "Done after follow-up.");

    // Verify `AGENTS.md` was injected into the system prompt on Turn 1.
    let turn1_msgs = &mock_driver.recorded_messages()[0];
    assert!(turn1_msgs[0]
        .content_str()
        .unwrap_or_default()
        .contains("RULE_42: Always verify tests."));

    // Verify PlanModeMiddleware allowed `echo safe_read_ok` and rejected `git reset --hard`.
    let turn3_msgs = &mock_driver.recorded_messages()[2];
    let allowed_echo = turn3_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("safe_read_ok"))
            .unwrap_or(false)
    });
    let rejected_reset = turn3_msgs.iter().any(|m| {
        m.content_str()
            .map(|c| c.contains("[Middleware:plan-mode]"))
            .unwrap_or(false)
    });
    assert!(allowed_echo, "PlanModeMiddleware must allow read-only echo");
    assert!(
        rejected_reset,
        "PlanModeMiddleware must reject mutating git reset"
    );
}
