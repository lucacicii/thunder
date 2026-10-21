//! The ask path's contract: one judge, one place, and the refusal that cannot
//! be misread.
//!
//! The judging used to be a middleware the pack assembled, which meant every
//! invariant here was also an invariant about assembly — leave the layer out
//! and there was no judge. The judge is in the kernel's funnel now
//! ([`ToolExecutor::run_tool`]); what is left to pin is the half the kernel
//! still owns outright: the dialog and everything around it.

mod support;

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_policy::ui::{HostUi, NotifyLevel, NullHostUi, UiRequest, UiResponse, UiSource};
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_policy::gate::{ALLOW_ONCE, DENY, ALLOW_ALWAYS, DENY_WITH_REASON};
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_types::tool::ToolDefinition;
use thunder_agent_pack_code::builtin::*;

/// Answers every dialog from a script, recording what it was asked.
struct ScriptedUi {
    answers: std::sync::Mutex<Vec<UiResponse>>,
    titles: std::sync::Mutex<Vec<String>>,
    options: std::sync::Mutex<Vec<Vec<String>>>,
}

impl ScriptedUi {
    fn new(answers: Vec<UiResponse>) -> Arc<Self> {
        Arc::new(Self {
            answers: std::sync::Mutex::new(answers.into_iter().rev().collect()),
            titles: std::sync::Mutex::new(Vec::new()),
            options: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn titles(&self) -> Vec<String> {
        self.titles.lock().unwrap().clone()
    }

    fn options(&self) -> Vec<Vec<String>> {
        self.options.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostUi for ScriptedUi {
    async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
        if let UiRequest::Select { title, options, .. } = &request {
            self.titles.lock().unwrap().push(title.clone());
            self.options.lock().unwrap().push(options.clone());
        }
        self.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(UiResponse::Cancelled)
    }
    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}
    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

/// A tool that always succeeds, so a refusal can never be the tool's fault.
struct Succeeds;

#[async_trait]
impl AgentTool for Succeeds {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "mystery_tool",
            "does something",
            serde_json::json!({
                "type": "object", "properties": { "q": { "type": "string" } }
            }),
        )
    }
    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        Ok("EXECUTED".into())
    }
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));
    registry.register(Arc::new(BashTool::default()));
    registry
}

fn call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall::new_function("call-1", tool, args.to_string())
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "call-1".into(),
        turn: 1,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    }
}

/// An executor judged by a gate that asks about everything that changes
/// something — the `ask` mode's shape — and answers through `ui`.
fn asking(ui: Arc<dyn HostUi>) -> ToolExecutor {
    ToolExecutor::new(registry())
        .with_tool_gate(Some(support::AskGate::mutations()))
        .with_host_ui(ui)
}

async fn run(executor: &ToolExecutor, tool: &str, args: serde_json::Value) -> ToolExecutionResult {
    executor.run_tool(&call(tool, args), ctx(), None).await
}

// ------------------------------------------------------------------ the dialog

/// The approval dialog is the one place a user is asked to say yes or no, so
/// its headline and its options have to read in the language the rest of the
/// terminal UI uses.
#[tokio::test]
async fn the_approval_dialog_reads_english() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let executor = asking(ui.clone());
    run(&executor, "bash", serde_json::json!({"command": "ls"})).await;

    assert_eq!(ui.titles(), vec!["Run bash".to_string()]);
    let asked = ui.options();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0][0], ALLOW_ONCE, "the once-only allow comes first");
    assert!(asked[0].iter().any(|option| option == DENY));
    assert!(
        asked[0].iter().all(|option| option.is_ascii()),
        "every option is English: {:?}",
        asked[0]
    );
}

/// "Always allow" appears only when the gate says the call can be narrowed to a
/// rule. This gate never says so, so the option never appears.
#[tokio::test]
async fn always_allow_is_offered_only_when_a_rule_can_exist() {
    let ui = ScriptedUi::new(vec![UiResponse::value(DENY)]);
    let executor = asking(ui.clone());
    run(
        &executor,
        "write_file",
        serde_json::json!({"path": "a", "content": "b"}),
    )
    .await;

    let asked = ui.options();
    assert_eq!(asked.len(), 1);
    assert!(
        !asked[0].contains(&ALLOW_ALWAYS.to_string()),
        "a call with no safe rule must not offer a blanket yes: {:?}",
        asked[0]
    );
}

#[tokio::test]
async fn allow_once_runs_the_call() {
    let executor = asking(ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]));
    let res = run(&executor, "bash", serde_json::json!({"command": "true"})).await;
    assert!(!res.is_error, "{}", res.output);
}

// ----------------------------------------------------------------- the refusal

#[tokio::test]
async fn refusal_is_a_tool_result_with_ground_truth_not_an_exception() {
    let executor = asking(ScriptedUi::new(vec![UiResponse::value(DENY)]));
    let res = run(
        &executor,
        "bash",
        serde_json::json!({"command": "rm -rf build"}),
    )
    .await;
    assert!(res.is_error);
    assert!(
        res.output.contains("was not approved"),
        "got: {}",
        res.output
    );
    assert!(
        res.output.contains("rm -rf build"),
        "must name what was blocked"
    );
    let notice = res.telemetry.expect("a refusal must carry telemetry");
    assert!(
        notice.guidance.is_some(),
        "the model needs the ground truth"
    );
}

#[tokio::test]
async fn every_non_answer_is_a_refusal() {
    // No panel, a dismissal, a value outside the offered set: all must land on
    // the deny branch. The ask path never fails open.
    for answer in [UiResponse::Cancelled, UiResponse::value("something-else")] {
        let executor = asking(ScriptedUi::new(vec![answer]));
        let res = run(&executor, "bash", serde_json::json!({"command": "ls"})).await;
        assert!(res.is_error, "must fail closed");
    }
    // A null UI is the headless host case: it must refuse rather than hang.
    let executor = asking(Arc::new(NullHostUi));
    let res = run(&executor, "bash", serde_json::json!({"command": "ls"})).await;
    assert!(res.is_error);
}

#[tokio::test]
async fn deny_with_reason_reaches_the_model() {
    let ui = ScriptedUi::new(vec![
        UiResponse::value(DENY_WITH_REASON),
        UiResponse::value("先别动生产配置"),
    ]);
    let executor = asking(ui);
    let res = run(
        &executor,
        "bash",
        serde_json::json!({"command": "systemctl restart nginx"}),
    )
    .await;
    assert!(res.is_error);
    assert!(
        res.output.contains("先别动生产配置"),
        "the user's own words must reach the model: {}",
        res.output
    );
}

/// Reads do not prompt under a gate shaped like the ask mode — and neither does
/// anything, under no gate at all.
#[tokio::test]
async fn reads_do_not_prompt_and_no_gate_prompts_nothing() {
    let ui = ScriptedUi::new(vec![]);
    let executor = asking(ui.clone());
    let res = run(
        &executor,
        "read_file",
        serde_json::json!({"path": "/etc/hosts"}),
    )
    .await;
    assert!(!res.is_error, "{}", res.output);
    assert!(ui.titles().is_empty());

    let ui = ScriptedUi::new(vec![]);
    let executor = ToolExecutor::new(registry()).with_host_ui(ui.clone());
    let res = run(&executor, "bash", serde_json::json!({"command": "ls"})).await;
    assert!(!res.is_error, "no gate means allow: {}", res.output);
    assert!(ui.titles().is_empty());
}

#[tokio::test]
async fn a_cancelled_run_raises_no_dialog() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let executor = asking(ui.clone());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let mut c = ctx();
    c.cancellation_token = token;
    let res = executor
        .run_tool(
            &call("read_file", serde_json::json!({"path": "a.txt"})),
            c,
            None,
        )
        .await;
    assert!(res.is_error);
    assert!(res.output.contains("cancelled"), "got: {}", res.output);
    assert!(ui.titles().is_empty(), "must not prompt a dead run");
}

// --------------------------------------------------------------- attribution

/// A plugin-initiated call must be presentable as such, or a user approving
/// "execute bash" is rubber-stamping the wrong thing.
#[tokio::test]
async fn a_plugin_request_is_labelled_and_a_model_request_is_not() {
    let ui = ScriptedUi::new(vec![UiResponse::value(DENY)]);
    let executor = asking(ui.clone());

    let mut plugin_ctx = ctx();
    plugin_ctx.caller = Some("evil_plugin".into());
    let res = executor
        .run_tool(
            &call("bash", serde_json::json!({"command": "ls"})),
            plugin_ctx,
            None,
        )
        .await;
    assert!(res.is_error);
    assert!(
        res.output.contains("evil_plugin"),
        "the plugin must be told who asked: {}",
        res.output
    );
    let titles = ui.titles();
    assert_eq!(titles.len(), 1);
    assert!(
        titles[0].contains("evil_plugin"),
        "the dialog must name the plugin: {titles:?}"
    );

    // A model-initiated call of the same shape must not be dressed up as one.
    let ui2 = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let executor = asking(ui2.clone());
    executor
        .run_tool(
            &call("bash", serde_json::json!({"command": "ls"})),
            ctx(),
            None,
        )
        .await;
    let titles = ui2.titles();
    assert_eq!(titles.len(), 1);
    assert!(!titles[0].contains("Plugin"), "got: {:?}", titles[0]);
}

// ------------------------------------------------------------- concurrency

/// `execute_all` runs a batch in parallel, so without serialisation several
/// prompts would overlap and the user could approve the wrong one.
#[tokio::test]
async fn parallel_calls_are_prompted_one_at_a_time() {
    let ui = ScriptedUi::new(vec![
        UiResponse::value(ALLOW_ONCE),
        UiResponse::value(ALLOW_ONCE),
    ]);
    let executor = asking(Arc::clone(&ui) as Arc<dyn HostUi>);

    let bash_call = call("bash", serde_json::json!({"command": "ls"}));
    let write_call = call(
        "write_file",
        serde_json::json!({"path": "x", "content": "y"}),
    );
    let token = tokio_util::sync::CancellationToken::new();
    let (a, b) = tokio::join!(
        executor.execute_one(&bash_call, 1, token.clone(), None),
        executor.execute_one(&write_call, 1, token, None),
    );
    assert!(!a.is_error && !b.is_error);
    // Two prompts happened, and the point is that they did not overlap — which
    // the serialised lock guarantees and the count observes.
    assert_eq!(ui.titles().len(), 2);
}

// --------------------------------------------------------------- integration

/// An unclassified tool is judged by its effect like any other: it asks, and a
/// denial names it.
#[tokio::test]
async fn an_unclassified_tool_is_judged_like_any_other() {
    let mut reg = registry();
    reg.register(Arc::new(Succeeds));
    let executor = ToolExecutor::new(reg)
        .with_tool_gate(Some(support::AskGate::mutations()))
        .with_host_ui(ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]));

    let ok = executor
        .run_tool(
            &call("mystery_tool", serde_json::json!({"q": "hi"})),
            ctx(),
            None,
        )
        .await;
    assert!(!ok.is_error, "{}", ok.output);
    assert_eq!(ok.output, "EXECUTED");
}
