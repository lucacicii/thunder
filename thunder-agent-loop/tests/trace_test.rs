//! One record per tool call, whatever the call's fate.
//!
//! The property this file exists to pin is structural, not statistical: the
//! kernel's funnel has a single exit, so a call that is cancelled, refused by
//! the guard, refused by a gate, declined by the user, never answered, or run to
//! completion all leave exactly one record — and there is no configuration that
//! turns the record off.
//!
//! Redaction is pinned here too. A trace is a durable artifact that travels to
//! disk, and the args and output of a real call are where credentials live.

use async_trait::async_trait;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::tools::trace::{AskAnswer, Decision, DenyBy, ToolTrace, ToolTraceSink};
use thunder_agent_loop::types::gate::{
    ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest, ALLOW_ALWAYS, ALLOW_ONCE, DENY,
};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::policy::{Caller, ToolEffect};
use thunder_agent_loop::types::scope::{Scope, ToolScope};
use thunder_agent_loop::types::tool::{
    AgentTool, ToolDefinition, ToolExecutionContext, ToolExecutionResult,
};
use thunder_agent_loop::types::ui::{HostUi, UiRequest, UiResponse, UiSource};

// ───────────────────────────────── fixtures ─────────────────────────────────

/// Collects every trace the executor publishes.
#[derive(Default)]
struct RecordingSink {
    traces: Mutex<Vec<ToolTrace>>,
}

impl RecordingSink {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn traces(&self) -> Vec<ToolTrace> {
        self.traces.lock().clone()
    }

    fn only(&self) -> ToolTrace {
        let mut traces = self.traces();
        assert_eq!(traces.len(), 1, "exactly one record per call: {traces:#?}");
        traces.remove(0)
    }
}

impl ToolTraceSink for RecordingSink {
    fn record(&self, trace: &ToolTrace) {
        self.traces.lock().push(trace.clone());
    }
}

/// A tool that echoes, or fails, on request.
struct Stub;

#[async_trait]
impl AgentTool for Stub {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "stub",
            "does a thing",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "fail": { "type": "boolean" }
                }
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        if args.get("fail").and_then(|v| v.as_bool()) == Some(true) {
            return Err("the tool failed on purpose".into());
        }
        Ok(format!("ECHO {args}"))
    }
}

/// A gate with a fixed verdict.
struct FixedGate {
    verdict: Box<dyn Fn(&ToolGateRequest<'_>) -> GateVerdict + Send + Sync>,
    label: &'static str,
}

impl ToolGate for FixedGate {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        (self.verdict)(request)
    }
    fn label(&self) -> &str {
        self.label
    }
}

/// The shape a mode's "allow" takes: a plain answer, or one attributed to a rule
/// the user already agreed to.
fn allow_gate() -> Arc<dyn ToolGate> {
    Arc::new(FixedGate {
        verdict: Box::new(|_| GateVerdict::Allow),
        label: "test-mode",
    })
}

fn rule_gate(rule: &'static str) -> Arc<dyn ToolGate> {
    Arc::new(FixedGate {
        verdict: Box::new(move |_| GateVerdict::AllowByRule {
            rule: rule.to_string(),
        }),
        label: "test-mode",
    })
}

fn ask_gate() -> Arc<dyn ToolGate> {
    Arc::new(FixedGate {
        verdict: Box::new(|request| {
            GateVerdict::Ask(ApprovalPrompt {
                title: format!("Run {}", request.tool),
                detail: "stub".to_string(),
                allow_always: true,
            })
        }),
        label: "ask-mode",
    })
}

fn deny_gate() -> Arc<dyn ToolGate> {
    Arc::new(FixedGate {
        verdict: Box::new(|_| GateVerdict::Deny {
            reason: "not in this mode".to_string(),
        }),
        label: "deny-mode",
    })
}

/// Marks everything critical, to exercise the guard's other list.
struct CriticalScope;

impl ToolScope for CriticalScope {
    fn scope_of(&self, _tool: &str, _effect: ToolEffect, _args: &serde_json::Value) -> Scope {
        Scope::Critical
    }
}

struct ScriptedUi {
    answers: Mutex<Vec<UiResponse>>,
}

impl ScriptedUi {
    fn new(answers: Vec<UiResponse>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().rev().collect()),
        })
    }
}

#[async_trait]
impl HostUi for ScriptedUi {
    async fn request(&self, _source: UiSource, _request: UiRequest) -> UiResponse {
        self.answers.lock().pop().unwrap_or(UiResponse::Cancelled)
    }
    fn notify(
        &self,
        _source: UiSource,
        _message: &str,
        _level: thunder_agent_loop::types::ui::NotifyLevel,
    ) {
    }
    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

/// An executor with the stub registered, a sink installed the way the loop
/// installs one, and whatever judgement the caller wants.
fn executor(sink: Arc<RecordingSink>, gate: Option<Arc<dyn ToolGate>>) -> ToolExecutor {
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(Stub));
    let mut executor = ToolExecutor::new(registry)
        .with_trace_ids(Some("sess-1".to_string()), Some("task-1".to_string()));
    if let Some(gate) = gate {
        executor = executor.with_tool_gate(Some(gate));
    }
    executor.install_run_trace(sink);
    executor
}

fn ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "call-1".into(),
        turn: 3,
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        ..Default::default()
    }
}

fn call(tool: &str, args: serde_json::Value) -> ToolCall {
    ToolCall::new_function("call-1", tool, args.to_string())
}

async fn run(executor: &ToolExecutor, tool: &str, args: serde_json::Value) -> ToolExecutionResult {
    executor.run_tool(&call(tool, args), ctx(), None).await
}

// ────────────────────────────── the eight exits ──────────────────────────────

/// Every way a call can end, and the verdict the record must carry for it.
#[tokio::test]
async fn every_exit_path_records_a_trace() {
    // 1. Allowed with no gate.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    let trace = sink.only();
    assert_eq!(trace.decision, Decision::Allow);
    assert_eq!(trace.mode, None, "no gate means no mode to name");

    // 2. Allowed by a standing rule.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(rule_gate("git status")));
    run(&exec, "stub", serde_json::json!({"command": "git status"})).await;
    let trace = sink.only();
    assert_eq!(
        trace.decision,
        Decision::AllowByRule {
            rule: "git status".into()
        }
    );
    assert_eq!(trace.mode.as_deref(), Some("test-mode"));

    // 3. The guard's dangerous-command list.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(allow_gate()));
    run(&exec, "bash", serde_json::json!({"command": "rm -rf /"})).await;
    let trace = sink.only();
    assert!(matches!(
        trace.decision,
        Decision::Deny {
            by: DenyBy::Guard,
            ..
        }
    ));
    assert!(trace.guard.is_some());
    assert_eq!(
        trace.mode, None,
        "the guard runs before the gate, so no mode was consulted"
    );

    // 4. The guard's critical-path list, reached through the host's scope.
    let sink = RecordingSink::new();
    let exec =
        executor(Arc::clone(&sink), Some(allow_gate())).with_tool_scope(Arc::new(CriticalScope));
    run(&exec, "stub", serde_json::json!({"path": "/etc/passwd"})).await;
    let trace = sink.only();
    assert_eq!(trace.scope, Scope::Critical);
    assert!(matches!(
        trace.decision,
        Decision::Deny {
            by: DenyBy::Guard,
            ..
        }
    ));

    // 5. Refused by the gate.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(deny_gate()));
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    let trace = sink.only();
    assert!(matches!(
        trace.decision,
        Decision::Deny {
            by: DenyBy::Gate,
            ..
        }
    ));
    assert_eq!(trace.mode.as_deref(), Some("deny-mode"));

    // 6. Asked, and allowed once.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(ask_gate()))
        .with_host_ui(ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]));
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    let trace = sink.only();
    assert_eq!(trace.decision, Decision::Asked);
    assert_eq!(trace.answer, Some(AskAnswer::AllowOnce));

    // 7. Asked, and the user said no.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(ask_gate()))
        .with_host_ui(ScriptedUi::new(vec![UiResponse::value(DENY)]));
    let res = run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    assert!(res.is_error);
    let trace = sink.only();
    assert_eq!(trace.decision, Decision::Asked);
    assert_eq!(trace.answer, Some(AskAnswer::Denied));

    // 8. Asked, and the user chose a standing rule.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(ask_gate()))
        .with_host_ui(ScriptedUi::new(vec![UiResponse::value(ALLOW_ALWAYS)]));
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    assert_eq!(sink.only().answer, Some(AskAnswer::AllowAlways));

    // 9. Asked into a panel nobody answers.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(ask_gate()))
        .with_host_ui(ScriptedUi::new(vec![UiResponse::Cancelled]));
    let res = run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    assert!(res.is_error, "silence is not consent");
    assert_eq!(sink.only().answer, Some(AskAnswer::Timeout));

    // 10. Asked with no panel at all — a different situation, said differently.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(ask_gate()));
    let res = run(&exec, "stub", serde_json::json!({"command": "ls"})).await;
    assert!(res.is_error);
    assert_eq!(sink.only().answer, Some(AskAnswer::NoUi));

    // 11. Cancelled before launch.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    let mut c = ctx();
    c.cancellation_token = token;
    let res = exec
        .run_tool(&call("stub", serde_json::json!({})), c, None)
        .await;
    assert!(res.is_error);
    assert_eq!(sink.only().decision, Decision::Cancelled);

    // 12. A tool nobody registered.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let res = run(&exec, "no_such_tool", serde_json::json!({})).await;
    assert!(res.is_error);
    assert_eq!(sink.only().tool, "no_such_tool");

    // 13. Arguments that do not parse.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let res = exec
        .run_tool(
            &ToolCall::new_function("call-1", "stub", "{not json"),
            ctx(),
            None,
        )
        .await;
    assert!(res.is_error);
    let trace = sink.only();
    assert_eq!(trace.arguments, serde_json::Value::Null);

    // 14. A tool that fails: the record says so.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let res = run(&exec, "stub", serde_json::json!({"fail": true})).await;
    assert!(res.is_error);
    assert!(sink.only().result.is_error);
}

/// The record identifies the run it belongs to, or a reader cannot join it
/// against anything.
#[tokio::test]
async fn a_trace_carries_its_run_and_its_call() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;

    let trace = sink.only();
    assert_eq!(trace.session_id.as_deref(), Some("sess-1"));
    assert_eq!(trace.task_id.as_deref(), Some("task-1"));
    assert_eq!(trace.tool_call_id, "call-1");
    assert_eq!(trace.turn, 3);
    assert_eq!(trace.caller, Caller::Model);
    assert_eq!(trace.effect, ToolEffect::Other);
    assert!(!trace.trace_id.is_empty());
    assert!(trace.finished_at_ms >= trace.started_at_ms);
}

/// A plugin's call is recorded as the plugin's: an audit that cannot tell who
/// asked is not an audit.
#[tokio::test]
async fn a_plugin_call_is_recorded_as_one() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let mut c = ctx();
    c.caller = Some("evil_plugin".into());
    exec.run_tool(&call("stub", serde_json::json!({})), c, None)
        .await;

    assert_eq!(
        sink.only().caller,
        Caller::Plugin("evil_plugin".to_string())
    );
}

// ──────────────────────────────── redaction ────────────────────────────────

/// Credentials reach a call under a name that says what they are, and the
/// record must not carry them.
#[tokio::test]
async fn arguments_under_a_secret_key_are_redacted() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    run(
        &exec,
        "stub",
        serde_json::json!({
            "command": "deploy",
            "api_key": "hunter2",
            "nested": { "Authorization": "Bearer abcdefghijklmnopqrst" },
        }),
    )
    .await;

    let trace = sink.only();
    let wire = serde_json::to_string(&trace).unwrap();
    assert!(!wire.contains("hunter2"), "the key's value is gone: {wire}");
    assert!(
        !wire.contains("Bearer abcdefghijklmnopqrst"),
        "and so is the one nested under a secret name: {wire}"
    );
    assert!(
        trace
            .redactions
            .iter()
            .any(|rule| rule.eq_ignore_ascii_case("key:api_key")),
        "the rule that fired is named: {:?}",
        trace.redactions
    );
    // The command itself is not a secret and must survive: a trace where
    // everything is redacted explains nothing.
    assert!(wire.contains("deploy"));
}

/// A secret that arrived as a value, not under a key, is still a secret.
#[tokio::test]
async fn secret_shaped_text_is_redacted_wherever_it_appears() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    run(
        &exec,
        "stub",
        serde_json::json!({
            "command": "git remote set-url origin https://x-access-token:ghp_0123456789abcdefghijklmnopqrstuvwx@github.com/a/b.git",
        }),
    )
    .await;

    let mut wire = serde_json::to_string(&sink.only()).unwrap();
    assert!(
        !wire.contains("ghp_0123456789abcdefghijklmnopqrstuvwx"),
        "a token inside free text is still a token: {wire}"
    );
    assert!(sink.only().redactions.iter().any(|r| r == "github_token"));
    wire.clear();
}

/// The output side is the real leak: `env` and a file read hand a credential to
/// a tool, and the tool's output is what lands in the record.
#[tokio::test]
async fn a_secret_in_the_output_never_reaches_the_record() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let out = run(&exec, "stub", serde_json::json!({"command": "env"})).await;
    // The tool echoed its arguments back, so the output carries whatever was
    // passed. A sink that redacted only arguments would ship this to disk.
    assert!(out.output.contains("env"));

    let trace = sink.only();
    assert!(trace.redactions.is_empty());

    // Now the same call with a credential in the output path.
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    run(
        &exec,
        "stub",
        serde_json::json!({"command": "echo AKIAIOSFODNN7EXAMPLE"}),
    )
    .await;
    let trace = sink.only();
    assert!(
        !trace.result.output_preview.contains("AKIAIOSFODNN7EXAMPLE"),
        "the output is the leak that matters: got: {}",
        trace.result.output_preview
    );
    assert!(
        trace.redactions.iter().any(|r| r == "aws_access_key_id"),
        "the rule that fired is named: {:?}",
        trace.redactions
    );
}

/// A document argument is kept as a digest and a preview: the file is already on
/// disk, and a trace that embeds every file written is a trace nobody reads.
#[tokio::test]
async fn a_document_argument_is_hashed_not_kept() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let body = "a".repeat(4000);
    run(
        &exec,
        "stub",
        serde_json::json!({"path": "big.txt", "content": body}),
    )
    .await;

    let trace = sink.only();
    let content = &trace.arguments["content"];
    assert!(content.is_object(), "the body became a summary: {content}");
    assert_eq!(content["bytes"], 4000);
    assert_eq!(
        content["digest"].as_str().map(str::len),
        Some(16),
        "a stable digest, not the text"
    );
    assert!(
        content["preview"].as_str().unwrap().chars().count() < 300,
        "the preview is short: {}",
        content["preview"]
    );
    assert!(trace.redactions.iter().any(|r| r == "document:content"));
}

/// The full output is not embedded: a tool that returns a megabyte must not put
/// a megabyte in the record.
#[tokio::test]
async fn output_is_previewed_and_digested() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), None);
    let big = "x".repeat(50_000);
    run(&exec, "stub", serde_json::json!({"command": big})).await;

    let trace = sink.only();
    assert!(
        trace.result.output_preview.chars().count() < 2200,
        "previewed: {}",
        trace.result.output_preview.chars().count()
    );
    assert!(trace.result.output_preview.contains("truncated"));
    assert_eq!(trace.result.output_digest.len(), 16);
    assert!(trace.result.result_bytes > 1000);
}

// ─────────────────────────── it cannot be turned off ───────────────────────────

/// There is no switch. A call is recorded because the funnel has one exit, not
/// because a host remembered to ask for it.
#[tokio::test]
async fn a_trace_cannot_be_disabled() {
    let sink = RecordingSink::new();
    // Deliberately built without a host sink, exactly as a run that wants no
    // extras would be, then given the run's own sink the way the loop gives it.
    let mut registry = ToolRegistry::new(64 * 1024, Duration::from_secs(5));
    registry.register(Arc::new(Stub));
    let exec = ToolExecutor::new(registry)
        .with_trace_sink(None)
        .with_trace_ids(None, None);
    exec.install_run_trace(sink.clone());

    run(&exec, "stub", serde_json::json!({})).await;
    assert_eq!(sink.traces().len(), 1, "no configuration turns this off");
    // With no ids to stamp, the record is still complete enough to read.
    let trace = sink.only();
    assert_eq!(trace.session_id, None);
    assert_eq!(trace.task_id, None);
}

/// A refused call is traced for the same reason an allowed one is: the question
/// a trace answers is "why", and a refusal is the answer people ask for.
#[tokio::test]
async fn the_refusal_reason_is_in_the_record() {
    let sink = RecordingSink::new();
    let exec = executor(Arc::clone(&sink), Some(deny_gate()));
    run(&exec, "stub", serde_json::json!({"command": "ls"})).await;

    let Decision::Deny { reason, by } = sink.only().decision else {
        panic!("expected a denial");
    };
    assert_eq!(by, DenyBy::Gate);
    assert_eq!(reason, "not in this mode");
}
