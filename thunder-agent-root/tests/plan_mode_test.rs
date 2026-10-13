//! Plan mode, end to end through the gate and the two tools it registers.
//!
//! Plan mode is the one mode with a *procedure* rather than only a table: the
//! agent cannot write, may write exactly one file, and leaves only when the
//! user says so. Every test here drives the real funnel
//! ([`ToolExecutor::run_tool`]) with a real [`ModePlugin`] installed, because
//! the contract is about what the model experiences, not about an internal
//! verdict.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition};
use thunder_agent_pack_code::builtin::{ReadFileTool, WriteFileTool};
use thunder_agent_root::plugin::ThunderPlugin;
use thunder_agent_root::plugins::mode::{
    Mode, ModePlugin, APPROVE_AND_AUTO_ACCEPT, APPROVE_BUT_ASK, APPROVE_FULLY_AUTONOMOUS,
    EXIT_PLAN_MODE_TOOL, KEEP_PLANNING, WRITE_PLAN_TOOL,
};

/// A panel that answers from a script and records every request it was handed.
struct ScriptedUi {
    answers: std::sync::Mutex<Vec<UiResponse>>,
    /// `(kind, title)` of every request, in order.
    seen: std::sync::Mutex<Vec<(String, String)>>,
    options: std::sync::Mutex<Vec<Vec<String>>>,
}

impl ScriptedUi {
    fn new(answers: Vec<UiResponse>) -> Arc<Self> {
        Arc::new(Self {
            answers: std::sync::Mutex::new(answers.into_iter().rev().collect()),
            seen: std::sync::Mutex::new(Vec::new()),
            options: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn seen(&self) -> Vec<(String, String)> {
        self.seen.lock().unwrap().clone()
    }

    fn options(&self) -> Vec<Vec<String>> {
        self.options.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl HostUi for ScriptedUi {
    async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
        let (title, options) = match &request {
            UiRequest::Review { title, options, .. } => (title.clone(), options.clone()),
            UiRequest::Select { title, options, .. } => (title.clone(), options.clone()),
            UiRequest::Input { title, .. } => (title.clone(), Vec::new()),
            UiRequest::Confirm { title, .. } => (title.clone(), Vec::new()),
            UiRequest::Editor { title, .. } => (title.clone(), Vec::new()),
        };
        self.seen
            .lock()
            .unwrap()
            .push((request.kind().to_string(), title));
        self.options.lock().unwrap().push(options);
        self.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(UiResponse::Cancelled)
    }

    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}

    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

/// A tool that succeeds and claims the plan effect, to prove the exemption is
/// keyed on the *name* and not on what a tool says about itself.
struct ClaimsPlanEffect;

#[async_trait::async_trait]
impl AgentTool for ClaimsPlanEffect {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("sneaky_writer", "writes something", serde_json::json!({}))
    }

    fn effect(&self) -> thunder_agent_loop::types::policy::ToolEffect {
        thunder_agent_loop::types::policy::ToolEffect::PlanArtifact
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        Ok("EXECUTED".into())
    }
}

/// A workspace, a bound plan mode, and an executor judged by it.
fn fixture(mode: Mode, session_id: &str) -> (tempfile::TempDir, Arc<ModePlugin>, ToolExecutor) {
    let dir = tempfile::tempdir().expect("tempdir");
    let ws = dir
        .path()
        .canonicalize()
        .unwrap_or_else(|_| dir.path().to_path_buf());

    let plugin = ModePlugin::new(mode);
    plugin.bind_session(Some(ws.clone()), session_id);

    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    for tool in plugin.tools() {
        registry.register(tool);
    }
    // The workspace's ordinary tools, present so the refusals are about the
    // mode and not about a missing tool.
    registry.register(Arc::new(
        WriteFileTool::default().with_default_cwd(ws.clone()),
    ));
    registry.register(Arc::new(
        ReadFileTool::default().with_default_cwd(ws.clone()),
    ));
    registry.register(Arc::new(ClaimsPlanEffect));

    let executor = ToolExecutor::new(registry).with_tool_gate(Some(Arc::clone(&plugin) as _));
    (dir, plugin, executor)
}

fn plan_file(ws: &std::path::Path, session_id: &str) -> PathBuf {
    ws.join(".thunder")
        .join("plans")
        .join(format!("{session_id}.md"))
}

async fn run(
    executor: &ToolExecutor,
    ui: Arc<ScriptedUi>,
    tool: &str,
    args: serde_json::Value,
) -> ToolExecutionResult {
    let executor = executor.clone().with_host_ui(ui);
    let call = ToolCall::new_function("c1", tool, args.to_string());
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    executor.run_tool(&call, ctx, None).await
}

// ------------------------------------------------------------ the closed door

/// The refusal the model reads must name the way out. Every tool is registered
/// in 0.4, so the model *will* see `write_file` in plan mode and try it; a
/// refusal that does not name `write_plan` / `exit_plan_mode` is read as "try
/// again, differently" and produces a retry loop.
#[test]
fn the_write_refusal_names_the_way_out() {
    let (_dir, plugin, _exec) = fixture(Mode::Plan, "s1");
    let verdict = thunder_agent_loop::types::gate::ToolGate::decide(
        &*plugin,
        &thunder_agent_loop::types::gate::ToolGateRequest {
            tool: "write_file",
            arguments: &serde_json::json!({"path": "a.txt", "content": "x"}),
            effect: thunder_agent_loop::types::policy::ToolEffect::Write,
            scope: thunder_agent_loop::types::scope::Scope::InScope,
            caller: thunder_agent_loop::types::policy::Caller::Model,
            tool_call_id: "c1",
        },
    );
    match verdict {
        thunder_agent_loop::types::gate::GateVerdict::Deny { reason } => {
            assert!(reason.contains("plan mode"), "got: {reason}");
            assert!(reason.contains(WRITE_PLAN_TOOL), "got: {reason}");
            assert!(reason.contains(EXIT_PLAN_MODE_TOOL), "got: {reason}");
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[tokio::test]
async fn a_write_in_plan_mode_is_refused_without_asking() {
    let (dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    let ui = ScriptedUi::new(vec![]);
    let res = run(
        &exec,
        ui.clone(),
        "write_file",
        serde_json::json!({"path": "a.txt", "content": "x"}),
    )
    .await;

    assert!(res.is_error);
    assert!(
        res.output.contains(EXIT_PLAN_MODE_TOOL),
        "got: {}",
        res.output
    );
    assert!(
        ui.seen().is_empty(),
        "a plan-mode refusal is a verdict, not a question: {:?}",
        ui.seen()
    );
    assert!(
        !dir.path().join("a.txt").exists(),
        "nothing may have been written"
    );
}

/// The exemption is a name check, never an effect check: a tool that declares
/// `PlanArtifact` about itself buys nothing.
#[tokio::test]
async fn a_self_declared_plan_effect_is_not_an_exemption() {
    let (_dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    let res = run(
        &exec,
        ScriptedUi::new(vec![]),
        "sneaky_writer",
        serde_json::json!({}),
    )
    .await;
    assert!(res.is_error, "got: {}", res.output);
    assert!(!res.output.contains("EXECUTED"));
}

// -------------------------------------------------------------- the one write

#[tokio::test]
async fn write_plan_writes_the_sessions_plan_file() {
    let (dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    let res = run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan\n\n1. do the thing"}),
    )
    .await;

    assert!(!res.is_error, "{}", res.output);
    let written = plan_file(dir.path(), "s1");
    assert!(
        written.exists(),
        "the plan must land at {}",
        written.display()
    );
    let text = std::fs::read_to_string(&written).unwrap();
    assert!(text.contains("1. do the thing"));
    assert!(
        res.output.contains(EXIT_PLAN_MODE_TOOL),
        "the tool tells the model what to do next: {}",
        res.output
    );
}

#[tokio::test]
async fn write_plan_overwrites_the_previous_plan() {
    let (dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "first"}),
    )
    .await;
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "second"}),
    )
    .await;

    let text = std::fs::read_to_string(plan_file(dir.path(), "s1")).unwrap();
    assert_eq!(text, "second", "revising a plan replaces it");
}

/// A session id arrives from whoever opened the session, and the path jail is
/// gone — so the id is the injection point, and it must not be able to leave
/// the plans directory.
#[tokio::test]
async fn a_hostile_session_id_cannot_escape_the_plans_directory() {
    let (dir, _plugin, exec) = fixture(Mode::Plan, "../../../etc/evil");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "escaped"}),
    )
    .await;

    let plans = dir.path().join(".thunder").join("plans");
    let entries: Vec<PathBuf> = std::fs::read_dir(&plans)
        .expect("the plans directory exists")
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1, "exactly one plan file: {entries:?}");
    let name = entries[0]
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(
        !name.contains('/'),
        "the sanitised name carries no separator: {name}"
    );
    // Nothing anywhere above it.
    assert!(!dir.path().join("etc").exists(), "escaped upwards");
    assert!(!dir.path().parent().unwrap().join("etc").exists());
    assert_eq!(std::fs::read_to_string(&entries[0]).unwrap(), "escaped");
}

#[tokio::test]
async fn a_session_with_no_workspace_is_told_so() {
    let plugin = ModePlugin::new(Mode::Plan);
    plugin.bind_session(None, "s1");
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    for tool in plugin.tools() {
        registry.register(tool);
    }
    let exec = ToolExecutor::new(registry).with_tool_gate(Some(Arc::clone(&plugin) as _));

    let res = run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "x"}),
    )
    .await;
    assert!(res.is_error);
    assert!(
        res.output.contains("workspace"),
        "the message names the missing thing: {}",
        res.output
    );
}

// ------------------------------------------------------------- the way out

#[tokio::test]
async fn exit_plan_mode_without_a_plan_is_an_error_without_a_dialog() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    let ui = ScriptedUi::new(vec![UiResponse::value(APPROVE_FULLY_AUTONOMOUS)]);
    let res = run(
        &exec,
        ui.clone(),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;

    assert!(res.is_error, "got: {}", res.output);
    assert!(res.output.contains(WRITE_PLAN_TOOL), "got: {}", res.output);
    assert!(
        ui.seen().is_empty(),
        "there is nothing to review, so nobody is asked: {:?}",
        ui.seen()
    );
    assert_eq!(plugin.mode(), Mode::Plan, "the mode is untouched");
}

#[tokio::test]
async fn an_empty_plan_is_refused_the_same_way() {
    let (dir, plugin, exec) = fixture(Mode::Plan, "s1");
    let path = plan_file(dir.path(), "s1");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "   \n").unwrap();

    let ui = ScriptedUi::new(vec![UiResponse::value(APPROVE_FULLY_AUTONOMOUS)]);
    let res = run(
        &exec,
        ui.clone(),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;
    assert!(res.is_error, "got: {}", res.output);
    assert!(ui.seen().is_empty(), "no dialog for an empty plan");
    assert_eq!(plugin.mode(), Mode::Plan);
}

/// The dialog is a review of the plan, and its options are the modes the user
/// can land in — the model never picks one.
#[tokio::test]
async fn the_review_shows_the_plan_and_offers_the_modes() {
    let (dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan\n\nthe body"}),
    )
    .await;

    let ui = ScriptedUi::new(vec![UiResponse::value(APPROVE_BUT_ASK)]);
    run(
        &exec,
        ui.clone(),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;

    let seen = ui.seen();
    assert_eq!(seen.len(), 1, "one review, not a select: {seen:?}");
    assert_eq!(seen[0].0, "review");
    let asked = ui.options();
    assert_eq!(
        asked[0],
        vec![
            APPROVE_AND_AUTO_ACCEPT.to_string(),
            APPROVE_BUT_ASK.to_string(),
            APPROVE_FULLY_AUTONOMOUS.to_string(),
            KEEP_PLANNING.to_string(),
        ]
    );
    let _ = dir;
}

#[tokio::test]
async fn approval_flips_the_mode_and_says_which_one() {
    for (answer, expected) in [
        (APPROVE_AND_AUTO_ACCEPT, Mode::AcceptEdits),
        (APPROVE_BUT_ASK, Mode::Ask),
        (APPROVE_FULLY_AUTONOMOUS, Mode::Yolo),
    ] {
        let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
        run(
            &exec,
            ScriptedUi::new(vec![]),
            WRITE_PLAN_TOOL,
            serde_json::json!({"content": "# Plan"}),
        )
        .await;

        let res = run(
            &exec,
            ScriptedUi::new(vec![UiResponse::value(answer)]),
            EXIT_PLAN_MODE_TOOL,
            serde_json::json!({}),
        )
        .await;

        assert!(!res.is_error, "approval is not an error: {}", res.output);
        assert_eq!(plugin.mode(), expected, "answer '{answer}'");
        assert!(
            res.output.contains(expected.as_str()),
            "the model must be told which mode now judges it: {}",
            res.output
        );
    }
}

/// The flip lands on the very next call: the kernel asks the gate every time.
#[tokio::test]
async fn the_next_call_is_judged_by_the_new_mode() {
    let (_dir, _plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;
    run(
        &exec,
        ScriptedUi::new(vec![UiResponse::value(APPROVE_AND_AUTO_ACCEPT)]),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;

    // acceptEdits allows an in-scope write with no dialog. The scope is
    // `Unknown` here (no scope installed), so it asks — which is itself the
    // proof that plan mode is over.
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let res = run(
        &exec,
        ui.clone(),
        "write_file",
        serde_json::json!({"path": "a.txt", "content": "x"}),
    )
    .await;
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(ui.seen().len(), 1, "acceptEdits asks outside the scope");
}

#[tokio::test]
async fn declining_keeps_the_mode_and_carries_the_feedback_back() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;

    let ui = ScriptedUi::new(vec![
        UiResponse::value(KEEP_PLANNING),
        UiResponse::value("start with the parser, not the CLI"),
    ]);
    let res = run(
        &exec,
        ui.clone(),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;

    assert!(!res.is_error, "a decline is not a failure: {}", res.output);
    assert_eq!(plugin.mode(), Mode::Plan, "still planning");
    assert!(
        res.output.contains("start with the parser"),
        "the feedback is the point: {}",
        res.output
    );
    let kinds: Vec<String> = ui.seen().into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, vec!["review".to_string(), "input".to_string()]);
}

/// Declining without saying anything still keeps the mode, and the model is
/// told to ask rather than to guess.
#[tokio::test]
async fn declining_silently_still_keeps_the_mode() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;

    let ui = ScriptedUi::new(vec![
        UiResponse::value(KEEP_PLANNING),
        UiResponse::Cancelled,
    ]);
    let res = run(&exec, ui, EXIT_PLAN_MODE_TOOL, serde_json::json!({})).await;
    assert!(!res.is_error);
    assert_eq!(plugin.mode(), Mode::Plan);
    assert!(res.output.contains(WRITE_PLAN_TOOL), "got: {}", res.output);
}

/// A review nobody answers is not an error — the run is still in plan mode and
/// the model has to be told that, not told to retry the call.
#[tokio::test]
async fn an_unanswered_review_stays_in_plan_mode() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;

    let res = run(
        &exec,
        ScriptedUi::new(vec![UiResponse::Cancelled]),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(plugin.mode(), Mode::Plan);
    assert!(res.output.contains("plan mode"), "got: {}", res.output);
}

/// An answer outside the offered set changes nothing: a surprising value is
/// never a way to flip a mode.
#[tokio::test]
async fn an_unrecognised_answer_flips_nothing() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;

    let res = run(
        &exec,
        ScriptedUi::new(vec![UiResponse::value("sure, whatever")]),
        EXIT_PLAN_MODE_TOOL,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(plugin.mode(), Mode::Plan);
    assert!(res.output.contains("plan mode"), "got: {}", res.output);
}

/// A headless host has no panel: the review resolves as unanswered, so the
/// session stays in plan mode instead of assuming consent.
#[tokio::test]
async fn a_headless_host_stays_in_plan_mode() {
    let (_dir, plugin, exec) = fixture(Mode::Plan, "s1");
    run(
        &exec,
        ScriptedUi::new(vec![]),
        WRITE_PLAN_TOOL,
        serde_json::json!({"content": "# Plan"}),
    )
    .await;

    let call = ToolCall::new_function("c1", EXIT_PLAN_MODE_TOOL, "{}");
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    };
    let res = exec.run_tool(&call, ctx, None).await;
    assert!(!res.is_error, "{}", res.output);
    assert_eq!(plugin.mode(), Mode::Plan, "silence is not consent");
}
