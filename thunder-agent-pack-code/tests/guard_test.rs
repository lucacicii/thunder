//! The kernel's floor, at the level where it has to hold: the tool executor.
//!
//! The guard is not a pipeline layer, and that is deliberate — a layer can be
//! left out of an assembly, and every way of reaching a tool would then need to
//! remember it. It sits in the one method every call passes through, so what
//! these tests pin is that *no* entry point skips it, and that its refusals
//! cannot be argued away by a host.

mod support;

use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_policy::{Guard, ToolExecutionContext};
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::trace::GuardKind;
use thunder_agent_pack_code::builtin::*;
use tokio_util::sync::CancellationToken;

/// A scope that claims every call is inside the workspace, i.e. the most
/// permissive thing a host could install.
struct EverythingIsFine;

impl ToolScope for EverythingIsFine {
    fn scope_of(&self, _tool: &str, _effect: ToolEffect, _arguments: &serde_json::Value) -> Scope {
        Scope::InScope
    }
}

/// A scope that declares a target off limits.
struct OffLimits;

impl ToolScope for OffLimits {
    fn scope_of(&self, _tool: &str, _effect: ToolEffect, _arguments: &serde_json::Value) -> Scope {
        Scope::Critical
    }
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(BashTool::default()));
    registry.register(Arc::new(WriteFileTool::default()));
    registry
}

/// An executor with a judge that allows everything and no dialog to answer.
/// Anything refused here was refused by the guard — which is also why this test
/// uses a permissive *gate* rather than no gate: the floor must hold under the
/// most permissive thing a host could install, not only in the gate's absence.
fn permissive_executor(scope: Option<Arc<dyn ToolScope>>) -> ToolExecutor {
    let mut executor =
        ToolExecutor::new(registry()).with_tool_gate(Some(support::AskGate::never()));
    if let Some(scope) = scope {
        executor = executor.with_tool_scope(scope);
    }
    executor
}

fn ctx(id: &str) -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: id.to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        ..Default::default()
    }
}

fn command(id: &str, command: &str) -> ToolCall {
    ToolCall::new_function(id, "bash", json!({ "command": command }).to_string())
}

#[tokio::test]
async fn no_entry_point_skips_the_guard() {
    let executor = permissive_executor(None);

    // The context-aware path.
    let res = executor
        .run_tool(&command("c1", "rm -rf /"), ctx("c1"), None)
        .await;
    assert!(res.is_error, "execute_with_context: {}", res.output);

    // The convenience path a plugin reaches for.
    let res = executor
        .execute_one(
            &command("c2", "mkfs /dev/sda"),
            1,
            CancellationToken::new(),
            None,
        )
        .await;
    assert!(res.is_error, "execute_one: {}", res.output);

    // The parallel batch the loop itself uses.
    let results = executor
        .execute_all(
            &[
                command("c3", "dd if=/dev/zero of=/dev/sda"),
                command("c4", "echo hi"),
            ],
            1,
            CancellationToken::new(),
            None,
            None,
        )
        .await;
    assert_eq!(results.len(), 2);
    assert!(results[0].result.is_error, "{}", results[0].result.output);
    assert!(
        !results[1].result.is_error,
        "its neighbour is untouched: {}",
        results[1].result.output
    );
}

#[tokio::test]
async fn the_floor_holds_under_a_permissive_scope_and_a_permissive_mode() {
    // Both halves of the policy cut: the mode asks nothing, and the host says
    // "everything is inside the workspace".
    let executor = permissive_executor(Some(Arc::new(EverythingIsFine)));

    let res = executor
        .run_tool(&command("c1", "rm -rf /"), ctx("c1"), None)
        .await;

    assert!(res.is_error, "{}", res.output);
    assert!(
        res.output.contains("does not run it"),
        "the refusal must not read as something a setting could change: {}",
        res.output
    );
}

#[tokio::test]
async fn a_host_can_declare_a_target_off_limits() {
    let executor = permissive_executor(Some(Arc::new(OffLimits)));

    let res = executor
        .run_tool(
            &ToolCall::new_function(
                "c1",
                "write_file",
                json!({ "path": "anything.txt", "content": "x" }).to_string(),
            ),
            ctx("c1"),
            None,
        )
        .await;

    assert!(res.is_error, "{}", res.output);
    assert!(
        res.output.contains("off limits"),
        "the model is told the target is protected, not that it lacks permission: {}",
        res.output
    );
    assert!(
        !std::path::Path::new("anything.txt").exists(),
        "and nothing was written"
    );
}

#[tokio::test]
async fn a_cancelled_call_never_reaches_the_guard_or_a_tool() {
    let executor = permissive_executor(None);
    let token = CancellationToken::new();
    token.cancel();

    let res = executor
        .execute_one(&command("c1", "echo hi"), 1, token, None)
        .await;

    assert!(res.is_error);
    assert!(
        res.output.contains("cancelled"),
        "cancellation is reported as itself, not as a command problem: {}",
        res.output
    );
}

/// The guard's `GuardHit` is the trace's too — one description of one refusal,
/// so a reader of the trace does not have to reconstruct it.
#[tokio::test]
async fn the_refusal_names_a_guard_kind() {
    let hit = Guard::check(
        "bash",
        &json!({ "command": "rm -rf /" }),
        ToolEffect::Exec,
        Scope::InScope,
    )
    .expect("refused");
    assert_eq!(hit.kind, GuardKind::DangerousCommand);

    let hit = Guard::check(
        "write_file",
        &json!({ "path": ".git/HEAD" }),
        ToolEffect::Write,
        Scope::Critical,
    )
    .expect("refused");
    assert_eq!(hit.kind, GuardKind::CriticalPath);
    assert_eq!(hit.detail, ".git/HEAD");
}
