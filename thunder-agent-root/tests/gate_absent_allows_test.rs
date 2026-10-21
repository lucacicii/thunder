//! The red line for headless and embedded hosts: no gate installed means
//! everything the kernel's guard allows runs.
//!
//! `thunder-agent-runtime` installs no policy and answers no dialogs, so "no
//! gate" has to be permissive by construction — not permissive because
//! something defaulted to allow, but because there is nothing to ask. The
//! companion case (a permissive *gate*) belongs to the pack's guard tests: only
//! the floor survives it.

use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_pack_code::builtin::*;
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::ui::NullHostUi;
use thunder_agent_policy::ToolExecutionContext;

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(BashTool::default()));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));
    registry
}

fn ctx(id: &str) -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: id.to_string(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    }
}

#[tokio::test]
async fn without_a_gate_everything_the_guard_allows_runs() {
    // No gate, and a panel that answers nothing: if anything asked, this run
    // would be dead. The whole point is that nothing does.
    let executor = ToolExecutor::new(registry()).with_host_ui(Arc::new(NullHostUi));

    let res = executor
        .run_tool(
            &ToolCall::new_function("c1", "bash", r#"{"command":"echo headless"}"#),
            ctx("c1"),
            None,
        )
        .await;
    assert!(!res.is_error, "no gate means allow: {}", res.output);

    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("out.txt");
    let res = executor
        .run_tool(
            &ToolCall::new_function(
                "c2",
                "write_file",
                serde_json::json!({"path": target, "content": "x"}).to_string(),
            ),
            ctx("c2"),
            None,
        )
        .await;
    assert!(!res.is_error, "{}", res.output);
    assert!(target.exists());

    // ... and the floor is still the floor. The guard does not depend on the
    // gate either existing or not existing.
    let res = executor
        .run_tool(
            &ToolCall::new_function("c3", "bash", r#"{"command":"rm -rf /"}"#),
            ctx("c3"),
            None,
        )
        .await;
    assert!(res.is_error, "the guard holds with no gate installed");
    assert!(res.output.contains("does not run it"), "{}", res.output);
}

/// Uninstalling the gate from a live executor is the same state, reached from
/// the other direction — which is what "mode is an unloadable plugin" means.
#[tokio::test]
async fn removing_the_gate_returns_to_the_gate_absent_state() {
    let installed = ToolExecutor::new(registry())
        .with_tool_gate(Some(Arc::new(DenyEverything)))
        .with_host_ui(Arc::new(NullHostUi));
    let res = installed
        .run_tool(
            &ToolCall::new_function("c1", "bash", r#"{"command":"echo x"}"#),
            ctx("c1"),
            None,
        )
        .await;
    assert!(res.is_error, "a denying gate denies: {}", res.output);

    let uninstalled = ToolExecutor::new(registry());
    let res = uninstalled
        .run_tool(
            &ToolCall::new_function("c2", "bash", r#"{"command":"echo x"}"#),
            ctx("c2"),
            None,
        )
        .await;
    assert!(
        !res.is_error,
        "and without it, the call runs: {}",
        res.output
    );
}

use thunder_agent_policy::gate::{GateVerdict, ToolGate, ToolGateRequest};

struct DenyEverything;

impl ToolGate for DenyEverything {
    fn decide(&self, _request: &ToolGateRequest<'_>) -> GateVerdict {
        GateVerdict::Deny {
            reason: "no".to_string(),
        }
    }
}
