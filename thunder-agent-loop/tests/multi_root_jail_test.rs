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
