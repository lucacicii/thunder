//! Multi-root jail integration: extra workspace roots grant the same
//! read/write standing as the primary workspace through the full onion
//! pipeline (PermissionGuard → SecurityGuard → ResourceGuard → Transaction).

use serde_json::json;
use std::sync::Arc;
use thunder_agent_loop::tools::builtin::{ReadFileTool, WriteFileTool};
use thunder_agent_loop::tools::middleware::ToolPipeline;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::config::{MiddlewareConfig, Permission};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::ToolExecutionContext;
use thunder_agent_loop::AgentLoop;
use tokio_util::sync::CancellationToken;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn ctx(id: &str) -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: id.to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        ..Default::default()
    }
}

#[tokio::test]
async fn write_file_lands_in_extra_root_via_transaction() {
    let ws = temp_dir("thunder_multiroot_ws");
    let repo = temp_dir("thunder_multiroot_repo");

    let mut registry = ToolRegistry::default();
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));

    let pipeline = ToolPipeline::configured(
        ws.clone(),
        std::slice::from_ref(&repo),
        registry,
        None,
        &MiddlewareConfig::default(),
        Permission::Bash,
        None,
        None,
    );

    // Absolute write into the referenced repository.
    let target = repo.join("src").join("main.rs");
    let call = ToolCall::new_function(
        "call_w1",
        "write_file",
        json!({ "path": target.to_string_lossy(), "content": "fn main() {}" }).to_string(),
    );
    let res = pipeline.execute(&call, &ctx("call_w1"), None).await;
    assert!(
        !res.is_error,
        "write into extra root must pass: {}",
        res.output
    );
    assert!(
        res.telemetry
            .as_ref()
            .map(|t| t.layer == "Transaction")
            .unwrap_or(false),
        "atomic write telemetry expected"
    );

    // The file is really on disk with the expected content.
    let on_disk = std::fs::read_to_string(&target).expect("file written through transaction");
    assert_eq!(on_disk, "fn main() {}");

    // Read it back through the same pipeline.
    let call = ToolCall::new_function(
        "call_r1",
        "read_file",
        json!({ "path": target.to_string_lossy() }).to_string(),
    );
    let res = pipeline.execute(&call, &ctx("call_r1"), None).await;
    assert!(
        !res.is_error,
        "read from extra root must pass: {}",
        res.output
    );

    // Writing outside every root is still blocked.
    let call = ToolCall::new_function(
        "call_w2",
        "write_file",
        json!({ "path": "/etc/thunder-multiroot-probe", "content": "x" }).to_string(),
    );
    let res = pipeline.execute(&call, &ctx("call_w2"), None).await;
    assert!(res.is_error, "out-of-jail write must be blocked");
    assert!(res.output.contains("escapes all allowed workspace roots"));

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&repo);
}

#[tokio::test]
async fn agent_loop_rebuilds_pipeline_with_extra_roots() {
    // `with_id` / `register_tool` rebuild the tool executor; extra roots must
    // survive those rebuilds (they come from AgentConfig).
    let ws = temp_dir("thunder_multiroot_ws2");
    let repo = temp_dir("thunder_multiroot_repo2");

    let config = thunder_agent_loop::types::config::AgentConfig::new("test-model")
        .with_workspace_dir(ws.clone())
        .with_extra_workspace_roots([repo.clone()])
        .without_transactions();

    let probe = repo.join("Cargo.toml");
    std::fs::write(&probe, "[package]").unwrap();

    let mut agent = AgentLoop::new(config).with_id("multiroot_unit");
    agent.register_tool(Arc::new(ReadFileTool::default()));

    let call = ToolCall::new_function(
        "call_r2",
        "read_file",
        json!({ "path": repo.join("Cargo.toml").to_string_lossy() }).to_string(),
    );
    let res = agent
        .tool_executor()
        .pipeline()
        .execute(&call, &ctx("call_r2"), None)
        .await;
    assert!(
        !res.is_error,
        "extra root must survive executor rebuilds: {}",
        res.output
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&repo);
}

/// End to end through the real pipeline and the real `bash` tool: the escape
/// seen in a live session (`cd <outside> && python3 - <<PY … open(p,"w") … PY`)
/// must not reach the filesystem, while the same work inside a root succeeds.
#[tokio::test]
async fn bash_cannot_write_outside_roots_via_cd_and_interpreter() {
    use thunder_agent_loop::tools::builtin::BashTool;

    let ws = temp_dir("thunder_bashjail_ws");
    let outside = temp_dir("thunder_bashjail_outside");
    let _ = std::fs::remove_file(outside.join("leak.txt"));

    let mut registry = ToolRegistry::default();
    // Same wiring as the TUI / host: bash runs in the workspace root.
    registry.register(Arc::new(BashTool::default().with_default_cwd(ws.clone())));
    let pipeline = ToolPipeline::configured(
        ws.clone(),
        &[],
        registry,
        None,
        &MiddlewareConfig::default(),
        Permission::Bash,
        None,
        None,
    );

    let run = |id: &'static str, command: String| {
        let call = ToolCall::new_function(id, "bash", json!({ "command": command }).to_string());
        let pipeline = &pipeline;
        async move { pipeline.execute(&call, &ctx(id), None).await }
    };

    let escape = format!(
        "cd {} && python3 - <<'PY'\nimport io\nio.open(\"leak.txt\",\"w\").write(\"leaked\")\nPY",
        outside.display()
    );
    let res = run("call_e1", escape).await;
    assert!(res.is_error, "cd+python write outside must be blocked: {}", res.output);
    assert!(res.output.contains("Shell write target"), "{}", res.output);
    assert!(!outside.join("leak.txt").exists(), "nothing may be written outside");

    let res = run("call_e2", format!("cd {} && touch leak.txt", outside.display())).await;
    assert!(res.is_error);
    assert!(!outside.join("leak.txt").exists());

    // The same interpreter write inside the workspace is fine and really lands.
    let res = run(
        "call_ok",
        "python3 -c \"open('inside.txt','w').write('ok')\"".to_string(),
    )
    .await;
    assert!(!res.is_error, "in-jail interpreter write must pass: {}", res.output);
    assert_eq!(std::fs::read_to_string(ws.join("inside.txt")).unwrap(), "ok");

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}
