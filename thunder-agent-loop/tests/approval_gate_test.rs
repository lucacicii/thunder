//! The approval gate's *position* in the onion is a security property, not a
//! preference. These tests pin the order so a future refactor cannot quietly
//! move the gate in front of the permission guard (which would let a user
//! approve their way past the role's tier) or behind the transaction layer
//! (which would stage files for a call that is about to be refused).

use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::middleware::{ApprovalGate, ToolPipeline};
use thunder_agent_loop::tools::registry::ToolRegistry;

fn names(pipeline: &ToolPipeline) -> Vec<String> {
    pipeline
        .middlewares()
        .iter()
        .map(|m| m.name().to_string())
        .collect()
}

fn pipeline_with_gate() -> ToolPipeline {
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(BashTool::default()));
    let policy = SessionPolicy::new(PermissionMode::Ask);
    let gate = ApprovalGate::new(policy, Arc::new(NullHostUi), Permission::Bash);
    ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(gate),
    )
}

#[test]
fn the_gate_sits_inside_the_guard_and_outside_the_transaction() {
    let pipeline = pipeline_with_gate();
    let order = names(&pipeline);
    println!("layer order: {order:?}");

    let guard = order
        .iter()
        .position(|n| n == "PermissionGuardMiddleware")
        .expect("guard present");
    let gate = order
        .iter()
        .position(|n| n == "ApprovalGate")
        .expect("gate installed");
    let tx = order
        .iter()
        .position(|n| n == "TransactionMiddleware")
        .expect("transaction present");

    assert!(
        guard < gate,
        "the gate must be inside the tier ceiling: {order:?}"
    );
    assert!(gate < tx, "the gate must be outside staging: {order:?}");
}

#[test]
fn a_pipeline_without_a_guard_refuses_to_install_the_gate() {
    // Belt and braces: `ToolPipeline::configured` always installs the guard, so
    // this exercises `insert_middleware_after` directly. A gate with no tier
    // check in front of it would be the only thing guarding the shell.
    let registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    let mut pipeline = ToolPipeline::from_registry(registry);
    let policy = SessionPolicy::new(PermissionMode::Ask);
    let gate = ApprovalGate::new(policy, Arc::new(NullHostUi), Permission::Bash);
    assert!(
        !pipeline.insert_middleware_after("PermissionGuardMiddleware", gate),
        "inserting behind a missing anchor must fail loudly, not append"
    );
    assert!(!pipeline.has_middleware("ApprovalGate"));
}

#[test]
fn no_gate_means_the_pipeline_is_unchanged() {
    let registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        None,
    );
    assert!(!pipeline.has_middleware("ApprovalGate"));
}

/// A plugin tool under the default mode must still run.
///
/// This is the compatibility guard. The gate classifies an unrecognised tool as
/// write-worthy (fail-safe), so had the default mode been `ask`, every plugin
/// and MCP tool call would raise a dialog — and in a headless host, where the
/// UI always cancels, would be refused outright. That would have broken every
/// existing run on upgrade.
#[tokio::test]
async fn plugin_tools_are_not_gated_by_default() {
    // Default mode = no gate at all, which is what a stock `run_task` gets.
    assert_eq!(PermissionMode::default(), PermissionMode::Yolo);

    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    let policy = SessionPolicy::new(PermissionMode::default());
    // Installed anyway, to prove the *policy* is what allows it — not merely the
    // gate's absence.
    let gate = ApprovalGate::new(policy, Arc::new(NullHostUi), Permission::Bash);
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(gate),
    );

    let call = ToolCall::new_function("c1", "some_plugin_tool", r#"{"q":"hello"}"#);
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let result = pipeline.execute(&call, &ctx, None).await;
    // The registry has no such tool, so the terminal refuses it — but it got
    // *there*, which is the point: the gate did not intercept. An approval
    // refusal would have said "was not approved" instead.
    assert!(
        result.output.contains("not found"),
        "the call must reach the registry unmediated: {}",
        result.output
    );
    assert!(!result.output.contains("was not approved"));
}

/// The flip side: opting into `ask` *does* gate plugin tools, because an
/// unrecognised tool is treated as write-worthy.
#[tokio::test]
async fn opting_into_ask_gates_plugin_tools() {
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    let policy = SessionPolicy::new(PermissionMode::Ask);
    // Null UI: every dialog is declined.
    let gate = ApprovalGate::new(policy, Arc::new(NullHostUi), Permission::Bash);
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(gate),
    );

    let call = ToolCall::new_function("c1", "some_plugin_tool", r#"{"q":"hello"}"#);
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let result = pipeline.execute(&call, &ctx, None).await;
    assert!(result.is_error, "ask mode must gate an unclassified tool");
    assert!(
        result.output.contains("was not approved"),
        "got: {}",
        result.output
    );
}

#[tokio::test]
async fn a_read_only_role_is_refused_before_any_dialog() {
    // The end-to-end claim: with the gate installed, a write under a read-only
    // role never reaches the UI. The NullHostUi would answer "cancelled" anyway,
    // so this asserts on the *message* — a refusal by the guard, not by the gate.
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    let policy = SessionPolicy::new(PermissionMode::Ask);
    let gate = ApprovalGate::new(policy, Arc::new(NullHostUi), Permission::Read);
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Read,
        Some(gate),
    );

    let call = ToolCall::new_function("c1", "write_file", r#"{"path":"x","content":"y"}"#);
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let result = pipeline.execute(&call, &ctx, None).await;
    assert!(result.is_error);
    assert!(
        result.output.contains("not available in the current role"),
        "the guard must speak first, got: {}",
        result.output
    );
    assert_ne!(result.output, "EXECUTED");
}
