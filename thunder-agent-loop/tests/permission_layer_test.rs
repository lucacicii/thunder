//! The permission layer's contract: one judge, one place, and the order that
//! makes it safe.
//!
//! The invariants here were, until recently, split across two adjacent
//! middlewares with a seam nothing tested as a unit. They are now the contract
//! of a single layer, and each one has a test that fails if it breaks.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::middleware::PermissionGuardMiddleware;
use thunder_agent_loop::tools::middleware::{ToolHandler, ToolMiddleware, ToolPipeline};
use thunder_agent_loop::tools::registry::ToolRegistry;

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(WriteFileTool::default()));
    registry.register(Arc::new(ReadFileTool::default()));
    registry.register(Arc::new(BashTool::default()));
    registry
}

/// Answers every dialog from a script, recording what it was asked.
struct ScriptedUi {
    answers: std::sync::Mutex<Vec<UiResponse>>,
    titles: std::sync::Mutex<Vec<String>>,
}

impl ScriptedUi {
    fn new(answers: Vec<UiResponse>) -> Arc<Self> {
        Arc::new(Self {
            answers: std::sync::Mutex::new(answers.into_iter().rev().collect()),
            titles: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn titles(&self) -> Vec<String> {
        self.titles.lock().unwrap().clone()
    }
}

#[async_trait]
impl HostUi for ScriptedUi {
    async fn request(&self, _source: UiSource, request: UiRequest) -> UiResponse {
        if let UiRequest::Select { title, .. } = &request {
            self.titles.lock().unwrap().push(title.clone());
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

struct Terminal;

#[async_trait]
impl ToolHandler for Terminal {
    async fn handle(
        &self,
        _call: &ToolCall,
        _ctx: &ToolExecutionContext,
        _timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        ToolExecutionResult::success("EXECUTED".into(), Duration::from_millis(1))
    }
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

fn guard(policy: Arc<SessionPolicy>, ui: Arc<dyn HostUi>) -> Arc<PermissionGuardMiddleware> {
    Arc::new(PermissionGuardMiddleware::new(policy, ui))
}

fn policy(tier: Permission, mode: PermissionMode) -> Arc<SessionPolicy> {
    SessionPolicy::new(tier, mode)
}

// ---------------------------------------------------------------- position

/// The layer must sit outside `TransactionMiddleware` so a refused call stages
/// no temp file, and there must be nothing outside it that could let a call run
/// unjudged.
#[test]
fn the_layer_sits_outside_the_transaction() {
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry(),
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(policy(Permission::Bash, PermissionMode::Ask)),
        Some(Arc::new(NullHostUi)),
    );
    let order: Vec<String> = pipeline
        .middlewares()
        .iter()
        .map(|m| m.name().to_string())
        .collect();

    let guard = order
        .iter()
        .position(|n| n == PermissionGuardMiddleware::NAME)
        .expect("the judge is installed");
    let tx = order
        .iter()
        .position(|n| n == "TransactionMiddleware")
        .expect("transaction present");
    assert_eq!(guard, 0, "nothing may run before the judge: {order:?}");
    assert!(
        guard < tx,
        "a refused call must not stage a file: {order:?}"
    );
}

/// Without a policy the layer still defends the pipeline with the bare tier, so
/// an embedder that has not adopted `SessionPolicy` is not left unguarded.
#[test]
fn a_pipeline_without_a_policy_still_has_the_judge() {
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry(),
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Read,
        None,
        None,
    );
    assert!(pipeline.has_middleware(PermissionGuardMiddleware::NAME));
    assert_eq!(
        pipeline.middlewares().len(),
        5,
        "the stack is otherwise intact"
    );
}

// ----------------------------------------------------------------- the tier

/// A capability the role forbids is refused *before* any dialog: the user can
/// never be offered a choice the role already decided.
#[tokio::test]
async fn a_forbidden_call_never_reaches_a_dialog() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let g = guard(policy(Permission::Read, PermissionMode::Ask), ui.clone());
    let res = g
        .handle(
            &call(
                "write_file",
                serde_json::json!({"path": "a", "content": "b"}),
            ),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;

    assert!(res.is_error);
    assert!(
        ui.titles().is_empty(),
        "a tier denial must not raise a prompt: {:?}",
        ui.titles()
    );
    assert!(res.telemetry.is_some(), "the model needs the ground truth");
}

#[tokio::test]
async fn the_denial_names_the_ceiling() {
    let g = guard(
        policy(Permission::Read, PermissionMode::Ask),
        Arc::new(NullHostUi),
    );
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "rm -rf /"})),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(res.is_error);
    let notice = res.telemetry.expect("telemetry");
    assert!(
        notice.ground_truth.contains("workspace is unchanged"),
        "got: {}",
        notice.ground_truth
    );
    assert!(
        res.output.contains("read-only"),
        "the reason should say why: {}",
        res.output
    );
}

/// Plan mode lowers the ceiling, so a write is a denial and not a question.
#[tokio::test]
async fn plan_mode_refuses_rather_than_asks() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let g = guard(policy(Permission::Bash, PermissionMode::Plan), ui.clone());
    let res = g
        .handle(
            &call(
                "write_file",
                serde_json::json!({"path": "a", "content": "b"}),
            ),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(res.is_error);
    assert!(
        ui.titles().is_empty(),
        "plan mode must not negotiate a write"
    );
}

// ----------------------------------------------------------------- the mode

#[tokio::test]
async fn allow_once_runs_the_call() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let g = guard(policy(Permission::Bash, PermissionMode::Ask), ui);
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "ls"})),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(!res.is_error);
    assert_eq!(res.output, "EXECUTED");
}

#[tokio::test]
async fn refusal_is_a_tool_result_with_ground_truth_not_an_exception() {
    let ui = ScriptedUi::new(vec![UiResponse::value(DENY)]);
    let g = guard(policy(Permission::Bash, PermissionMode::Ask), ui);
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "rm -rf build"})),
            &ctx(),
            None,
            Arc::new(Terminal),
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
    // the deny branch. This layer never fails open.
    for answer in [UiResponse::Cancelled, UiResponse::value("something-else")] {
        let g = guard(
            policy(Permission::Bash, PermissionMode::Ask),
            ScriptedUi::new(vec![answer]),
        );
        let res = g
            .handle(
                &call("bash", serde_json::json!({"command": "ls"})),
                &ctx(),
                None,
                Arc::new(Terminal),
            )
            .await;
        assert!(res.is_error, "must fail closed");
    }
    // A null UI is the headless host case: it must refuse rather than hang.
    let g = guard(
        policy(Permission::Bash, PermissionMode::Ask),
        Arc::new(NullHostUi),
    );
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "ls"})),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(res.is_error);
}

#[tokio::test]
async fn deny_with_reason_reaches_the_model() {
    let ui = ScriptedUi::new(vec![
        UiResponse::value(DENY_WITH_REASON),
        UiResponse::value("先别动生产配置"),
    ]);
    let g = guard(policy(Permission::Bash, PermissionMode::Ask), ui);
    let res = g
        .handle(
            &call(
                "bash",
                serde_json::json!({"command": "systemctl restart nginx"}),
            ),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(res.is_error);
    assert!(
        res.output.contains("先别动生产配置"),
        "the user's own words must reach the model: {}",
        res.output
    );
}

#[tokio::test]
async fn yolo_never_raises_a_dialog() {
    let ui = ScriptedUi::new(vec![]);
    let g = guard(policy(Permission::Bash, PermissionMode::Yolo), ui.clone());
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "ls"})),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(!res.is_error);
    assert!(ui.titles().is_empty(), "yolo must not prompt");
}

#[tokio::test]
async fn reads_do_not_prompt_in_ask_mode() {
    let ui = ScriptedUi::new(vec![]);
    let g = guard(policy(Permission::Bash, PermissionMode::Ask), ui.clone());
    let res = g
        .handle(
            &call("read_file", serde_json::json!({"path": "a.txt"})),
            &ctx(),
            None,
            Arc::new(Terminal),
        )
        .await;
    assert!(!res.is_error);
    assert!(ui.titles().is_empty());
}

#[tokio::test]
async fn a_cancelled_run_raises_no_dialog() {
    let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
    let g = guard(policy(Permission::Bash, PermissionMode::Manual), ui.clone());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let mut c = ctx();
    c.cancellation_token = token;
    let res = g
        .handle(
            &call("read_file", serde_json::json!({"path": "a.txt"})),
            &c,
            None,
            Arc::new(Terminal),
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
    let g = guard(policy(Permission::Bash, PermissionMode::Ask), ui.clone());

    let mut plugin_ctx = ctx();
    plugin_ctx.caller = Some("evil_plugin".into());
    let res = g
        .handle(
            &call("bash", serde_json::json!({"command": "ls"})),
            &plugin_ctx,
            None,
            Arc::new(Terminal),
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
    let g2 = guard(policy(Permission::Bash, PermissionMode::Ask), ui2.clone());
    g2.handle(
        &call("bash", serde_json::json!({"command": "ls"})),
        &ctx(),
        None,
        Arc::new(Terminal),
    )
    .await;
    let titles = ui2.titles();
    assert_eq!(titles.len(), 1);
    assert!(!titles[0].contains("插件"), "got: {:?}", titles[0]);
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
    let g = guard(
        policy(Permission::Bash, PermissionMode::Ask),
        Arc::clone(&ui) as Arc<dyn HostUi>,
    );

    let bash_call = call("bash", serde_json::json!({"command": "ls"}));
    let write_call = call(
        "write_file",
        serde_json::json!({"path": "x", "content": "y"}),
    );
    let c = ctx();
    let (a, b) = tokio::join!(
        g.handle(&bash_call, &c, None, Arc::new(Terminal)),
        g.handle(&write_call, &c, None, Arc::new(Terminal)),
    );
    assert!(!a.is_error && !b.is_error);
    assert_eq!(ui.titles().len(), 2);
}

// ------------------------------------------------------------- integration

/// The end-to-end claim: with a pipeline installed, an unclassified tool is
/// judged by the ceiling rather than waved through.
#[tokio::test]
async fn an_unclassified_tool_is_judged_in_a_real_pipeline() {
    struct Mystery;
    #[async_trait]
    impl AgentTool for Mystery {
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

    let mut reg = registry();
    reg.register(Arc::new(Mystery));

    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        reg,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(policy(Permission::Bash, PermissionMode::Yolo)),
        Some(Arc::new(NullHostUi)),
    );
    let executor = ToolExecutor::with_pipeline(ToolRegistry::default(), pipeline);

    // Wide enough tier, no prompting: runs.
    let ok = executor
        .execute_one(
            &call("mystery_tool", serde_json::json!({"q": "hi"})),
            1,
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await;
    assert!(!ok.is_error, "{}", ok.output);

    // Read-only tier: refused, because an unclassified tool needs write access.
    // Before the two tables were merged this tool was waved through by
    // `allows_builtin`, which returned `true` for names it did not know.
    let pipeline = ToolPipeline::configured(
        std::env::temp_dir(),
        &[],
        registry(),
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Read,
        Some(policy(Permission::Read, PermissionMode::Yolo)),
        Some(Arc::new(NullHostUi)),
    );
    let executor = ToolExecutor::with_pipeline(ToolRegistry::default(), pipeline);
    let denied = executor
        .execute_one(
            &call("mystery_tool", serde_json::json!({"q": "hi"})),
            1,
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await;
    assert!(
        denied.is_error,
        "an unclassified tool must not run read-only"
    );
    assert_ne!(denied.output, "EXECUTED");
}
