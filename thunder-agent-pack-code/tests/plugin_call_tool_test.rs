//! `ctx.callTool`: a plugin reaching a tool on its own initiative.
//!
//! The risk this file exists to pin down: an invoker that dispatches straight
//! to the registry would be a privilege escalation wearing a plugin's name —
//! no gate, no guard, no approval prompt. Every test here asserts that the call
//! goes through the *same* funnel a model-initiated call does.

mod support;

use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_policy::invoke::{ExecutorToolInvoker, NullToolInvoker, ToolInvocationContext, ToolInvoker};
use thunder_agent_policy::ui::{NotifyLevel, NullHostUi};
use thunder_agent_policy::gate::{ALLOW_ONCE, DENY};
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::gate::ToolGate;
use thunder_agent_pack_code::builtin::*;

/// The executor a host hands the invoker: real tools, an optional judge, an
/// optional panel.
fn executor(
    gate: Option<Arc<dyn ToolGate>>,
    ui: Option<Arc<dyn thunder_agent_policy::ui::HostUi>>,
    ws: &std::path::Path,
) -> ToolExecutor {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    // Registered the way a host registers them: a relative path in a call means
    // "relative to the workspace". A bare tool would resolve against the process
    // working directory — which is the thing this test would then be asserting.
    registry.register(Arc::new(
        WriteFileTool::default().with_default_cwd(ws.to_path_buf()),
    ));
    registry.register(Arc::new(
        ReadFileTool::default().with_default_cwd(ws.to_path_buf()),
    ));
    registry.register(Arc::new(
        BashTool::default().with_default_cwd(ws.to_path_buf()),
    ));

    let mut executor = ToolExecutor::new(registry);
    if let Some(gate) = gate {
        executor = executor.with_tool_gate(Some(gate));
    }
    if let Some(ui) = ui {
        executor = executor.with_host_ui(ui);
    }
    executor
}

fn invoker(executor: ToolExecutor, turn: usize) -> Arc<ExecutorToolInvoker> {
    Arc::new(ExecutorToolInvoker::new(executor).with_turn(turn))
}

fn ctx(plugin: &str) -> ToolInvocationContext {
    ToolInvocationContext {
        plugin_id: plugin.to_string(),
        session_id: Some("sess-1".into()),
        turn: Some(1),
    }
}

/// The baseline: a plugin may use the run's own authority. That is the honest
/// description of "a plugin running inside the agent", and denying it would
/// make `ctx.callTool` useless.
#[tokio::test]
async fn a_plugin_can_use_the_runs_own_authority() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let inv = invoker(executor(None, None, ws), 1);

    let out = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "by_plugin.txt", "content": "hi" }),
            &ctx("demo"),
        )
        .await
        .expect("write should run under a gate-less run");
    assert!(out.contains("by_plugin.txt") || !out.is_empty());
    assert!(
        ws.join("by_plugin.txt").exists(),
        "the file must really land"
    );
}

/// A plugin cannot escalate past the gate. This is the single most important
/// assertion in the file: without it, "the plugin runs inside the agent"
/// becomes "the plugin *is* the agent". Both shapes of refusal are pinned — a
/// gate that denies outright, and a gate that asks into a panel that cannot
/// answer.
#[tokio::test]
async fn a_plugin_cannot_escalate_past_the_gate() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();

    // A session whose gate denies everything: no dialog, just the refusal.
    let inv = invoker(executor(Some(Arc::new(support::DenyAll)), None, ws), 1);
    let err = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "escalated.txt", "content": "nope" }),
            &ctx("hostile"),
        )
        .await
        .expect_err("a denying gate must refuse a plugin's write");
    assert!(err.contains("was not approved"), "got: {err}");
    assert!(
        err.contains("hostile"),
        "the plugin must be told who asked: {err}"
    );
    assert!(
        !ws.join("escalated.txt").exists(),
        "nothing may have been written"
    );

    // A session whose gate asks, with no panel to answer: fail closed.
    let inv = invoker(
        executor(
            Some(support::AskGate::mutations() as Arc<dyn ToolGate>),
            Some(Arc::new(NullHostUi)),
            ws,
        ),
        1,
    );
    let err = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "escalated.txt", "content": "nope" }),
            &ctx("hostile"),
        )
        .await
        .expect_err("an unanswered dialog must refuse, not assume consent");
    assert!(err.contains("was not approved"), "got: {err}");
    assert!(!ws.join("escalated.txt").exists());
}

/// Nor past the kernel's guard. The floor that refuses `rm -rf /` sits below
/// the gate and below the pipeline, so it holds for a plugin-initiated call
/// exactly as it does for the model's — including with no gate at all.
#[tokio::test]
async fn a_plugin_cannot_run_a_guarded_command() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let inv = invoker(executor(None, None, ws), 1);

    let err = inv
        .invoke(
            "bash",
            serde_json::json!({ "command": "rm -rf /" }),
            &ctx("hostile"),
        )
        .await
        .expect_err("the guard must hold for plugin-initiated calls too");
    assert!(
        err.contains("does not run it"),
        "the refusal must say it is not a permission question: {err}"
    );
}

/// A plugin-initiated privileged call goes through the approval gate, and the
/// dialog names the plugin. Without the name, a user approving "Run bash" has
/// no way to know a plugin asked rather than the assistant.
#[tokio::test]
async fn a_plugin_initiated_call_is_gated_and_attributed() {
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use thunder_agent_policy::ui::{UiRequest, UiResponse, UiSource};

    struct Recorder {
        titles: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait]
    impl thunder_agent_policy::ui::HostUi for Recorder {
        async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
            if let UiRequest::Select { title, .. } = request {
                self.titles.lock().unwrap().push(title);
            }
            // The user says no.
            UiResponse::value(DENY)
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _key: &str, _text: Option<String>) {}
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let titles = Arc::new(StdMutex::new(Vec::new()));
    // A gate that asks about shell commands, which is the case the dialog —
    // and the attribution it owes the user — exists for.
    let inv = invoker(
        executor(
            Some(support::AskGate::always()),
            Some(Arc::new(Recorder {
                titles: Arc::clone(&titles),
            })),
            ws,
        ),
        1,
    );

    let err = inv
        .invoke(
            "bash",
            serde_json::json!({ "command": "echo hi" }),
            &ctx("evil_plugin"),
        )
        .await
        .expect_err("a denied call must surface as an error to the plugin");

    assert!(err.contains("was not approved"), "got: {err}");
    assert!(
        err.contains("evil_plugin"),
        "the plugin must be told who asked: {err}"
    );

    let titles = titles.lock().unwrap();
    assert_eq!(titles.len(), 1, "exactly one prompt");
    assert!(
        titles[0].contains("evil_plugin"),
        "the dialog must name the plugin so a user is not misled: {:?}",
        titles[0]
    );
}

/// A model-initiated call of the same shape must *not* be labelled as a plugin
/// request — otherwise the attribution is noise.
#[tokio::test]
async fn a_model_call_is_not_labelled_as_a_plugin() {
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use thunder_agent_policy::ui::{UiRequest, UiResponse, UiSource};

    struct Recorder {
        titles: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait]
    impl thunder_agent_policy::ui::HostUi for Recorder {
        async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
            if let UiRequest::Select { title, .. } = request {
                self.titles.lock().unwrap().push(title);
            }
            UiResponse::value(ALLOW_ONCE)
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _key: &str, _text: Option<String>) {}
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let titles = Arc::new(StdMutex::new(Vec::new()));
    let exec = executor(
        Some(support::AskGate::always()),
        Some(Arc::new(Recorder {
            titles: Arc::clone(&titles),
        })),
        ws,
    );

    let call = ToolCall::new_function("m1", "bash", r#"{"command":"echo hi"}"#);
    let res = exec
        .execute_one(&call, 1, tokio_util::sync::CancellationToken::new(), None)
        .await;
    assert!(!res.is_error, "allowed: {}", res.output);

    let titles = titles.lock().unwrap();
    assert_eq!(titles.len(), 1);
    assert!(
        !titles[0].contains("插件") && !titles[0].contains("Plugin"),
        "a model call must not be dressed up as a plugin request: {:?}",
        titles[0]
    );
}

/// A plugin asking for a tool that does not exist gets a plain "not found",
/// not a crash and not an empty success.
#[tokio::test]
async fn an_unknown_tool_is_an_ordinary_error() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let inv = invoker(executor(None, None, ws), 1);

    let err = inv
        .invoke("no_such_tool", serde_json::json!({}), &ctx("demo"))
        .await
        .expect_err("must not silently succeed");
    assert!(err.contains("not found"), "got: {err}");
}

/// With no invoker installed — a host that runs no agent — the refusal explains
/// itself, and names the caller, instead of surfacing "unsupported RPC method".
#[tokio::test]
async fn no_invoker_means_a_clear_refusal() {
    let err = NullToolInvoker
        .invoke("write_file", serde_json::json!({}), &ctx("demo"))
        .await
        .expect_err("no invoker means no call");
    assert!(err.contains("not callable"), "got: {err}");
    assert!(err.contains("demo"), "must name the caller: {err}");
}

/// A plugin call aborts when the owning run's cancellation token is triggered.
#[tokio::test]
async fn cancellation_propagates_to_plugin_invocations() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let cancel = tokio_util::sync::CancellationToken::new();

    let inv = Arc::new(
        ExecutorToolInvoker::new(executor(None, None, ws))
            .with_turn(1)
            .with_cancellation(cancel.clone()),
    );

    // Cancel before or during invocation
    cancel.cancel();

    let err = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "never_written.txt", "content": "data" }),
            &ctx("demo"),
        )
        .await
        .expect_err("cancelled run must refuse plugin tool execution");

    assert!(
        err.contains("cancelled") || err.contains("not run"),
        "error should indicate task cancellation: {err}"
    );
    assert!(
        !ws.join("never_written.txt").exists(),
        "no file should be written"
    );
}
