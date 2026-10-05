//! `ctx.callTool`: a plugin reaching a tool on its own initiative.
//!
//! The risk this file exists to pin down: an invoker that dispatches straight
//! to the registry would be a privilege escalation wearing a plugin's name —
//! no tier, no jail, no approval prompt. Every test here asserts that the call
//! goes through the *same* onion a model-initiated call does.

use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::middleware::ToolPipeline;
use thunder_agent_loop::tools::registry::ToolRegistry;

fn pipeline(tier: Permission, mode: ApprovalMode, ws: &std::path::Path) -> ToolPipeline {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));
    registry.register(Arc::new(BashTool::default()));

    ToolPipeline::configured(
        ws.to_path_buf(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        tier,
        Some(SessionPolicy::new(tier, mode)),
        Some(Arc::new(NullHostUi)),
    )
}

fn invoker(pipeline: ToolPipeline, turn: usize) -> Arc<PipelineToolInvoker> {
    // The pipeline owns its terminal handler, so the executor is rebuilt around
    // the same pipeline rather than around a second registry.
    let executor = ToolExecutor::with_pipeline(
        ToolRegistry::new(64 * 1024, Duration::from_secs(5)),
        pipeline,
    );
    Arc::new(PipelineToolInvoker::new(executor).with_turn(turn))
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
    let p = pipeline(Permission::Bash, ApprovalMode::Never, ws);
    let inv = invoker(p, 1);

    let out = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "by_plugin.txt", "content": "hi" }),
            &ctx("demo"),
        )
        .await
        .expect("write should run under a yolo bash run");
    assert!(out.contains("by_plugin.txt") || !out.is_empty());
    assert!(
        ws.join("by_plugin.txt").exists(),
        "the file must really land"
    );
}

/// A plugin must not be able to escalate past the run's tier. This is the single
/// most important assertion in the file: without it, "the plugin runs inside the
/// agent" becomes "the plugin *is* the agent".
#[tokio::test]
async fn a_plugin_cannot_escalate_past_the_tier() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    // Read-only role. No mode can change that, and no mode may reach a dialog
    // either — the ceiling is checked first, so a user is never offered a choice
    // the role already decided.
    for mode in ApprovalMode::ALL {
        let p = pipeline(Permission::Read, mode, ws);
        let inv = invoker(p, 1);

        let err = inv
            .invoke(
                "write_file",
                serde_json::json!({ "path": "escalated.txt", "content": "nope" }),
                &ctx("hostile"),
            )
            .await
            .expect_err("a read-only run must not be writable by a plugin");
        assert!(
            err.contains("was not approved"),
            "{} produced: {err}",
            mode.as_str()
        );
        assert!(
            err.contains("read-only"),
            "{} should say the ceiling was the cause: {err}",
            mode.as_str()
        );
        assert!(
            err.contains("hostile"),
            "the plugin must be told who asked: {err}"
        );
        assert!(
            !ws.join("escalated.txt").exists(),
            "{} must not have written anything",
            mode.as_str()
        );
    }
}

/// Nor past the path jail. A plugin asking for `/etc/passwd` is refused by the
/// same layer that refuses the model.
#[tokio::test]
async fn a_plugin_cannot_escape_the_path_jail() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let p = pipeline(Permission::Bash, ApprovalMode::Never, ws);
    let inv = invoker(p, 1);

    let err = inv
        .invoke(
            "write_file",
            serde_json::json!({ "path": "/tmp/escaped_by_plugin.txt", "content": "nope" }),
            &ctx("hostile"),
        )
        .await
        .expect_err("the jail must hold for plugin-initiated calls too");
    assert!(!err.contains("ok"), "got: {err}");
    assert!(
        !std::path::Path::new("/tmp/escaped_by_plugin.txt").exists(),
        "nothing may be written outside the workspace"
    );
}

/// A plugin-initiated privileged call goes through the approval gate, and the
/// dialog names the plugin. Without the name, a user approving "Run bash" has
/// no way to know a plugin asked rather than the assistant.
#[tokio::test]
async fn a_plugin_initiated_call_is_gated_and_attributed() {
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use thunder_agent_loop::types::ui::{UiResponse, UiSource};

    struct Recorder {
        titles: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait]
    impl thunder_agent_loop::types::ui::HostUi for Recorder {
        async fn request(
            &self,
            _source: UiSource,
            request: thunder_agent_loop::types::ui::UiRequest,
        ) -> UiResponse {
            if let thunder_agent_loop::types::ui::UiRequest::Select { title, .. } = request {
                self.titles.lock().unwrap().push(title);
            }
            // The user says no.
            UiResponse::value(DENY)
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _: &str, _: Option<String>) {}
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();

    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(BashTool::default()));
    let titles = Arc::new(StdMutex::new(Vec::new()));
    let pipeline = ToolPipeline::configured(
        ws.to_path_buf(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(SessionPolicy::new(
            Permission::Bash,
            ApprovalMode::Mutations,
        )),
        Some(Arc::new(Recorder {
            titles: Arc::clone(&titles),
        })),
    );
    let inv = invoker(pipeline, 1);

    let err = inv
        .invoke(
            "bash",
            serde_json::json!({ "command": "rm -rf /" }),
            &ctx("evil_plugin"),
        )
        .await
        .expect_err("a refused call must surface as an error to the plugin");

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
    use thunder_agent_loop::types::ui::{UiResponse, UiSource};

    struct Recorder {
        titles: Arc<StdMutex<Vec<String>>>,
    }
    #[async_trait]
    impl thunder_agent_loop::types::ui::HostUi for Recorder {
        async fn request(
            &self,
            _source: UiSource,
            request: thunder_agent_loop::types::ui::UiRequest,
        ) -> UiResponse {
            if let thunder_agent_loop::types::ui::UiRequest::Select { title, .. } = request {
                self.titles.lock().unwrap().push(title);
            }
            UiResponse::value(ALLOW_ONCE)
        }
        fn notify(&self, _: UiSource, _: &str, _: NotifyLevel) {}
        fn set_status(&self, _: &str, _: Option<String>) {}
    }

    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path();
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(BashTool::default()));
    let titles = Arc::new(StdMutex::new(Vec::new()));
    let pipeline = ToolPipeline::configured(
        ws.to_path_buf(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(SessionPolicy::new(
            Permission::Bash,
            ApprovalMode::Mutations,
        )),
        Some(Arc::new(Recorder {
            titles: Arc::clone(&titles),
        })),
    );

    let call = ToolCall::new_function("m1", "bash", r#"{"command":"echo hi"}"#);
    let ctx = ToolExecutionContext {
        tool_call_id: "m1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let res = pipeline.execute(&call, &ctx, None).await;
    assert!(!res.is_error, "allowed: {}", res.output);

    let titles = titles.lock().unwrap();
    assert_eq!(titles.len(), 1);
    assert!(
        !titles[0].contains("插件"),
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
    let p = pipeline(Permission::Bash, ApprovalMode::Never, ws);
    let inv = invoker(p, 1);

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
    let p = pipeline(Permission::Bash, ApprovalMode::Never, ws);
    let cancel = tokio_util::sync::CancellationToken::new();

    let executor =
        ToolExecutor::with_pipeline(ToolRegistry::new(64 * 1024, Duration::from_secs(5)), p);
    let inv = Arc::new(
        PipelineToolInvoker::new(executor)
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
