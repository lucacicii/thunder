//! The path boundary, end to end — and the ordinary work that used to be
//! refused by it.
//!
//! The jail this replaces answered "is this path inside the workspace" with a
//! refusal, which made the false positives unrecoverable: `rm -rf /tmp/build`
//! matched `rm -rf /`, a `$VAR` in a write target was presumed hostile, and a
//! `cd` into a variable-relative directory poisoned every later write on the
//! line. None of that was a question a user could answer.
//!
//! So the first half of this file is an *acceptance* contract: the calls that
//! were refused, and must now run or at worst ask. The second half is what
//! still refuses, and why — a dangerous command, in every mode, because that is
//! the one kind of call a yes cannot undo.

mod support;

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_pack_code::builtin::*;
use thunder_agent_pack_code::CodeScope;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("thunder_scope_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().unwrap_or(dir)
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));
    registry.register(Arc::new(BashTool::default()));
    registry
}

/// The whole stack as a host assembles it: pipeline, registry, and the scope the
/// kernel consults about paths.
fn executor(
    ws: &PathBuf,
    extra_roots: &[PathBuf],
    asking: bool,
    ui: Option<Arc<dyn HostUi>>,
) -> ToolExecutor {
    let gate: Option<Arc<dyn thunder_agent_loop::types::gate::ToolGate>> = if asking {
        Some(support::AskGate::mutations())
    } else {
        None
    };
    let mut executor = ToolExecutor::new(registry());
    if let Some(gate) = gate {
        executor = executor.with_tool_gate(Some(gate));
    }
    if let Some(ui) = ui {
        executor = executor.with_host_ui(ui);
    }
    executor.with_tool_scope(Arc::new(
        CodeScope::new(ws).with_extra_roots(extra_roots.to_vec()),
    ))
}

fn ctx(id: &str) -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: id.to_string(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    }
}

async fn run(
    executor: &ToolExecutor,
    id: &str,
    tool: &str,
    args: serde_json::Value,
) -> ToolExecutionResult {
    executor
        .run_tool(
            &ToolCall::new_function(id, tool, args.to_string()),
            ctx(id),
            None,
        )
        .await
}

/// The refusal markers. A call whose result contains none of these was neither
/// refused by a security layer nor denied by the user.
fn refused_by_security(output: &str) -> bool {
    [
        "Access Denied",
        "Execution Blocked",
        "escapes all allowed workspace roots",
        "Refused:",
        "Path traversal",
    ]
    .iter()
    .any(|marker| output.contains(marker))
}

// ───────────────────────────────────────────── the work that must not be refused

/// Each of these was refused before this change. Every one of them is an
/// ordinary thing to do, and the point of the redesign is that the worst outcome
/// is now a question.
#[tokio::test]
async fn the_calls_that_used_to_be_refused_are_not_any_more() {
    let ws = temp_dir("accept");
    // Yolo: nothing is asked, so anything that still refuses is the guard — and
    // the guard must refuse none of these.
    let executor = executor(&ws, &[], false, None);

    let commands = [
        // `rm -rf /` as a substring of a longer, harmless path.
        "rm -rf /tmp/thunder-soft-jail-probe && true",
        "echo done > /tmp/thunder-soft-jail-probe.log",
        "rm -f /tmp/thunder-soft-jail-probe.log",
        // A `$VAR` the parser could not resolve.
        "LOG=/tmp/thunder-soft-jail-probe.log && echo done > \"$LOG\" && rm -f \"$LOG\"",
        // `cd` into something that is not a literal path.
        "BUILD_DIR=/tmp && cd \"$BUILD_DIR\" && touch thunder-cd-probe && rm -f thunder-cd-probe",
        // `pushd`/`popd`: the old parser treated `popd` as unknowable and
        // refused everything after it.
        "pushd /tmp > /dev/null && popd > /dev/null && touch /tmp/thunder-popd-probe && rm -f /tmp/thunder-popd-probe",
        // A redirect inside a quoted string, and a field descriptor redirect.
        "echo \"error: wrote > /tmp/log\"",
        "ls /nonexistent 2> /dev/null || true",
        // The bare word of a dangerous command, quoted, in documentation.
        "grep -rn \"mkfs\" . 2> /dev/null || true",
        // An interpreter that reads a system file (was a "write escape").
        "python3 -c \"import os; os.system('cat /etc/hosts')\" > /dev/null",
    ];

    for command in commands {
        let res = run(
            &executor,
            "call_accept",
            "bash",
            json!({ "command": command }),
        )
        .await;
        assert!(
            !refused_by_security(&res.output),
            "`{command}` must not be refused: {}",
            res.output
        );
    }

    // A read outside the workspace is not a crime either.
    let res = run(
        &executor,
        "call_read",
        "read_file",
        json!({ "path": "/etc/hosts" }),
    )
    .await;
    assert!(
        !res.is_error,
        "reading a system file is ordinary work: {}",
        res.output
    );

    let _ = std::fs::remove_dir_all(&ws);
}

// ────────────────────────────────────────────────── what still works, unchanged

#[tokio::test]
async fn a_write_inside_the_workspace_lands_and_reports_itself() {
    let ws = temp_dir("inside");
    let executor = executor(&ws, &[], false, None);

    let target = ws.join("src").join("main.rs");
    let res = run(
        &executor,
        "call_w1",
        "write_file",
        json!({ "path": target.to_string_lossy(), "content": "fn main() {}" }),
    )
    .await;

    assert!(!res.is_error, "{}", res.output);
    assert_eq!(
        std::fs::read_to_string(&target).expect("written"),
        "fn main() {}"
    );

    let res = run(
        &executor,
        "call_r1",
        "read_file",
        json!({ "path": target.to_string_lossy() }),
    )
    .await;
    assert!(!res.is_error, "{}", res.output);

    let _ = std::fs::remove_dir_all(&ws);
}

#[tokio::test]
async fn an_extra_root_is_inside_the_workspace_too() {
    let ws = temp_dir("extra_ws");
    let repo = temp_dir("extra_repo");
    let executor = executor(&ws, std::slice::from_ref(&repo), false, None);

    let target = repo.join("Cargo.toml");
    let res = run(
        &executor,
        "call_w2",
        "write_file",
        json!({ "path": target.to_string_lossy(), "content": "[package]" }),
    )
    .await;
    assert!(!res.is_error, "{}", res.output);
    assert!(target.exists(), "the extra root is writable");

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&repo);
}

// ────────────────────────────────────────────── the question, where it belongs

/// Answers every dialog by denying, recording that it was asked at all.
#[derive(Debug)]
struct DenyingUi {
    asked: std::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl HostUi for DenyingUi {
    async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
        if let UiRequest::Select { title, .. } = &request {
            self.asked.lock().unwrap().push(title.clone());
        }
        UiResponse::value(DENY)
    }
    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

/// The core of the soft boundary: a write the pack considers out of scope is a
/// question the user can answer, not a wall. This is the difference the whole
/// change turns on.
#[tokio::test]
async fn an_out_of_scope_write_is_a_question_the_user_can_answer() {
    let ws = temp_dir("outside_ws");
    let outside = temp_dir("outside_target");
    let ui = Arc::new(DenyingUi {
        asked: std::sync::Mutex::new(Vec::new()),
    });
    // `Mutations` asks about anything that changes something.
    let executor = executor(&ws, &[], true, Some(ui.clone() as Arc<dyn HostUi>));

    let target = outside.join("probe.txt");
    let res = run(
        &executor,
        "call_out",
        "write_file",
        json!({ "path": target.to_string_lossy(), "content": "x" }),
    )
    .await;

    assert!(
        !refused_by_security(&res.output),
        "an out-of-scope write is not a security refusal: {}",
        res.output
    );
    assert!(
        res.output.contains("was not approved"),
        "it is the user's refusal that stopped it: {}",
        res.output
    );
    assert!(!target.exists(), "and nothing was written");
    assert_eq!(
        ui.asked.lock().unwrap().len(),
        1,
        "the user was asked exactly once"
    );

    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

// ──────────────────────────────────────────────────── what still has a floor

/// The kernel's guard, through the whole stack, in the mode that asks nothing.
#[tokio::test]
async fn a_dangerous_command_is_refused_in_every_mode() {
    let ws = temp_dir("danger");
    let executor = executor(&ws, &[], false, None);

    let res = run(
        &executor,
        "call_danger",
        "bash",
        json!({ "command": "rm -rf /" }),
    )
    .await;

    assert!(res.is_error, "{}", res.output);
    assert!(
        res.output.contains("does not run it"),
        "and says it is not a matter of permission: {}",
        res.output
    );
    assert_eq!(
        res.telemetry.as_ref().map(|t| t.layer.as_str()),
        Some("Guard"),
        "attributed to the guard, not to a policy the user could change"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

/// A repository's metadata is off limits in both directions of the question:
/// asking "may I rewrite .git/HEAD?" is a question that should not be asked.
#[tokio::test]
async fn repository_metadata_is_off_limits_without_asking() {
    let ws = temp_dir("gitws");
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::write(ws.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

    let ui = Arc::new(DenyingUi {
        asked: std::sync::Mutex::new(Vec::new()),
    });
    let executor = executor(&ws, &[], true, Some(ui.clone() as Arc<dyn HostUi>));

    let res = run(
        &executor,
        "call_git",
        "write_file",
        json!({ "path": ws.join(".git/HEAD").to_string_lossy(), "content": "x" }),
    )
    .await;

    assert!(res.is_error);
    assert!(
        res.telemetry.as_ref().map(|t| t.layer.as_str()) == Some("Guard"),
        "the guard refuses it: {}",
        res.output
    );
    assert!(
        ui.asked.lock().unwrap().is_empty(),
        "without asking — a yes here could not be undone"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/main\n",
        "and the file is untouched"
    );

    let _ = std::fs::remove_dir_all(&ws);
}
