//! Approval gate: the layer that asks a human before a tool call proceeds.
//!
//! # Position in the onion
//!
//! ```text
//! PermissionGuardMiddleware   tier ceiling  — hard deny, no prompt
//!   └─ ApprovalGate           prompting     — this file
//!        └─ SecurityGuard / ResourceGuard / Transaction / …
//! ```
//!
//! Two invariants make this placement the only safe one:
//!
//! * **Inside the guard.** A capability the role forbids never reaches a
//!   dialog, so a user cannot "approve" their way past the tier, and no prompt
//!   ever appears for something that was going to be refused anyway.
//! * **Outside Transaction.** A refused call stages nothing: no temp file, no
//!   partial write, no cleanup window.
//!
//! # Failure posture
//!
//! Every non-answer is a refusal: no UI, timeout, closed channel, or a
//! cancellation all land in [`Decision::Deny`]. The gate never fails open.
//!
//! # Concurrency
//!
//! Tool calls in one assistant message run in parallel
//! (`ToolExecutor::execute_all` uses `join_all`), so without serialisation a
//! batch of three writes would raise three overlapping dialogs and the user
//! could approve the wrong one. A mutex makes prompts strictly sequential.

use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::config::Permission;
use crate::types::message::ToolCall;
use crate::types::policy::{
    AllowRule, ApprovalRequest, Decision, SessionPolicy, ALLOW_ALWAYS, ALLOW_ONCE, DENY,
    DENY_WITH_REASON,
};
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use crate::types::ui::{HostUi, UiSource};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub struct ApprovalGate {
    /// Session-scoped mode + remembered rules. Read per call, which is what
    /// makes a mid-session mode switch take effect immediately.
    policy: Arc<SessionPolicy>,
    ui: Arc<dyn HostUi>,
    /// Serialises dialogs. See the module docs on parallel tool batches.
    prompt_lock: Mutex<()>,
    /// Ceiling in force for this run, for plan-mode denial wording.
    permission: Permission,
    /// How long to wait for the user before refusing.
    ///
    /// Kept short by default: a hung panel must not stall a turn indefinitely,
    /// and a user who has walked away should not be waited on forever.
    timeout: Duration,
}

impl ApprovalGate {
    pub const NAME: &'static str = "ApprovalGate";

    pub fn new(
        policy: Arc<SessionPolicy>,
        ui: Arc<dyn HostUi>,
        permission: Permission,
    ) -> Arc<Self> {
        Arc::new(Self {
            policy,
            ui,
            prompt_lock: Mutex::new(()),
            permission,
            timeout: Duration::from_secs(120),
        })
    }

    /// Override the wait for a human answer.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The policy this gate consults, so a host can flip the mode without
    /// rebuilding the pipeline.
    pub fn policy(&self) -> &Arc<SessionPolicy> {
        &self.policy
    }

    /// Refusal result, phrased for the model.
    ///
    /// Follows `PermissionGuardMiddleware`: the failure is a *tool result*, not
    /// an exception, and it carries a `SystemNotice` so the model learns the
    /// ground truth ("nothing was written") instead of assuming success and
    /// retrying the same thing.
    fn refuse(
        &self,
        call: &ToolCall,
        detail: &str,
        reason: &str,
        call_caller: Option<&str>,
        start: Instant,
    ) -> ToolExecutionResult {
        let tool = call.function.name.as_str();
        let mut message = format!("Error: '{tool}' was not approved");
        if let Some(caller) = call_caller.filter(|c| !c.is_empty()) {
            message.push_str(&format!(" (requested by plugin '{caller}')"));
        }
        if !detail.is_empty() {
            message.push_str(&format!(" — {detail}"));
        }
        message.push_str(&format!(". Reason: {reason}"));

        let mut guidance = format!(
            "The user declined this specific call{}. Do not retry it unchanged and do not look for \
             another tool that achieves the same effect — ask what they want instead.",
            if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            }
        );
        if self.permission == Permission::Read {
            guidance
                .push_str(" This run is read-only, so a write would be refused even if approved.");
        }

        let notice = SystemNotice::new(
            Self::NAME,
            format!("Blocked '{tool}' — the user did not approve it"),
            "The call never ran. The workspace is unchanged.",
        )
        .with_guidance(guidance);

        ToolExecutionResult::error(message, start.elapsed()).with_telemetry(notice)
    }
}

#[async_trait]
impl ToolMiddleware for ApprovalGate {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        _timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();
        let tool = call.function.name.as_str();
        // Providers hand arguments over as a JSON *string*; an unparseable one
        // must not become an empty object that silently matches a broad rule.
        let args: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

        // A cancelled run must not raise a dialog nobody will answer.
        if ctx.cancellation_token.is_cancelled() {
            return ToolExecutionResult::error(
                format!("Error: '{tool}' was not run because the task was cancelled"),
                start.elapsed(),
            );
        }

        let decision = self.policy.decide(tool, &args).await;
        let request = match decision {
            Decision::Allow => return next.handle(call, ctx, _timeout).await,
            Decision::Deny { reason } => {
                let detail = crate::types::policy::describe_call(tool, &args);
                return self.refuse(call, &detail, &reason, ctx.caller.as_deref(), start);
            }
            Decision::Ask(request) => request,
        };

        let outcome = self
            .prompt(&request, &args, ctx.caller_label(), &ctx.cancellation_token)
            .await;
        match outcome {
            PromptOutcome::AllowOnce => next.handle(call, ctx, _timeout).await,
            PromptOutcome::AllowAlways(rule) => {
                if let Some(rule) = rule {
                    self.policy.remember(rule).await;
                }
                next.handle(call, ctx, _timeout).await
            }
            PromptOutcome::Refuse(reason) => {
                let detail = crate::types::policy::describe_call(tool, &args);
                self.refuse(call, &detail, &reason, ctx.caller.as_deref(), start)
            }
        }
    }
}

enum PromptOutcome {
    AllowOnce,
    AllowAlways(Option<AllowRule>),
    Refuse(String),
}

impl ApprovalGate {
    /// Raise the dialog and interpret the answer.
    ///
    /// Serialised by `prompt_lock`, and raced against cancellation so a
    /// cancelled run releases the lock instead of holding the batch hostage.
    async fn prompt(
        &self,
        request: &ApprovalRequest,
        args: &serde_json::Value,
        caller: &str,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> PromptOutcome {
        let _serialised = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return PromptOutcome::Refuse("the task was cancelled".to_string());
            }
            guard = self.prompt_lock.lock() => guard,
        };

        // `ALLOW_ALWAYS` is offered only when a narrow rule can be derived from
        // the live arguments. A blanket "always allow this tool" is not
        // something this design grants — notably for plugin tools, whose
        // arguments carry no scoping the host can reason about.
        let rule = AllowRule::for_call(&request.tool, args);
        let options: Vec<String> = request
            .options
            .iter()
            .filter(|o| o.as_str() != ALLOW_ALWAYS || rule.is_some())
            .cloned()
            .collect();

        let timeout_ms = Some(self.timeout.as_millis() as u64);
        // Name the caller when it is not the model. A plugin acting on its own
        // initiative must not be presentable as something the assistant asked
        // for — that is the whole difference between an informed yes and a
        // rubber stamp.
        let title = if caller.is_empty() {
            request.title.clone()
        } else {
            format!("插件 {caller} 请求：{}", request.title)
        };
        let response = self
            .ui
            .request(
                UiSource::Host,
                crate::types::ui::UiRequest::Select {
                    title,
                    options: options.clone(),
                    timeout_ms,
                },
            )
            .await;

        let choice = match response.text() {
            Some(text) => text.to_string(),
            // Timeout, dismissal, or no panel at all. Fail closed, and say so
            // plainly so the model does not read silence as consent.
            None => return PromptOutcome::Refuse("no answer (timed out or dismissed)".to_string()),
        };

        match choice.as_str() {
            ALLOW_ONCE => PromptOutcome::AllowOnce,
            ALLOW_ALWAYS => match rule {
                Some(rule) => PromptOutcome::AllowAlways(Some(rule)),
                // Cannot happen: the option is filtered out above. Fail closed
                // anyway rather than trust the filter to stay in sync.
                None => PromptOutcome::Refuse(
                    "no safe 'always allow' rule exists for this call".to_string(),
                ),
            },
            DENY => PromptOutcome::Refuse("the user declined".to_string()),
            DENY_WITH_REASON => {
                let reason = self
                    .ui
                    .request(
                        UiSource::Host,
                        crate::types::ui::UiRequest::Input {
                            title: "拒绝原因（会告知 Agent）".to_string(),
                            placeholder: Some("例如：先别动生产配置".to_string()),
                            timeout_ms,
                        },
                    )
                    .await
                    .text()
                    .unwrap_or("")
                    .to_string();
                if reason.trim().is_empty() {
                    PromptOutcome::Refuse("the user declined without giving a reason".to_string())
                } else {
                    PromptOutcome::Refuse(format!("the user declined: {}", reason.trim()))
                }
            }
            // An answer outside the offered set is treated as a refusal: a
            // surprising value must never widen access.
            _ => PromptOutcome::Refuse(format!("unrecognised answer: {choice}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::policy::PermissionMode;
    use crate::types::ui::{NotifyLevel, UiResponse};
    use std::sync::Mutex as StdMutex;

    /// Answers each dialog from a script, recording what it was asked.
    struct ScriptedUi {
        answers: StdMutex<Vec<UiResponse>>,
        seen: StdMutex<Vec<(String, Vec<String>)>>,
    }

    impl ScriptedUi {
        fn new(answers: Vec<UiResponse>) -> Arc<Self> {
            Arc::new(Self {
                answers: StdMutex::new(answers.into_iter().rev().collect()),
                seen: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl HostUi for ScriptedUi {
        async fn request(
            &self,
            _source: UiSource,
            request: crate::types::ui::UiRequest,
        ) -> UiResponse {
            if let crate::types::ui::UiRequest::Select { title, options, .. } = &request {
                self.seen
                    .lock()
                    .unwrap()
                    .push((title.clone(), options.clone()));
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
            ToolExecutionResult::success("executed".into(), Duration::from_millis(1))
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

    fn gate(policy: Arc<SessionPolicy>, ui: Arc<dyn HostUi>) -> Arc<ApprovalGate> {
        ApprovalGate::new(policy, ui, Permission::Bash)
    }

    #[tokio::test]
    async fn allow_once_runs_the_call() {
        let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
        let g = gate(SessionPolicy::new(PermissionMode::Ask), ui.clone());
        let res = g
            .handle(
                &call("bash", serde_json::json!({"command": "ls"})),
                &ctx(),
                None,
                Arc::new(Terminal),
            )
            .await;
        assert!(!res.is_error);
        assert_eq!(res.output, "executed");
    }

    #[tokio::test]
    async fn refusal_is_a_tool_result_with_ground_truth_not_an_exception() {
        let ui = ScriptedUi::new(vec![UiResponse::value(DENY)]);
        let g = gate(SessionPolicy::new(PermissionMode::Ask), ui);
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
    async fn silence_is_a_refusal() {
        // No panel, no answer, timeout — all must land on the deny branch.
        for answer in [UiResponse::Cancelled, UiResponse::value("something-else")] {
            let ui = ScriptedUi::new(vec![answer]);
            let g = gate(SessionPolicy::new(PermissionMode::Ask), ui);
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
    }

    #[tokio::test]
    async fn deny_with_reason_reaches_the_model() {
        let ui = ScriptedUi::new(vec![
            UiResponse::value(DENY_WITH_REASON),
            UiResponse::value("先别动生产配置"),
        ]);
        let g = gate(SessionPolicy::new(PermissionMode::Ask), ui);
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
        let ui = ScriptedUi::new(vec![]); // would cancel if asked
        let g = gate(SessionPolicy::new(PermissionMode::Yolo), ui.clone());
        let res = g
            .handle(
                &call("bash", serde_json::json!({"command": "ls"})),
                &ctx(),
                None,
                Arc::new(Terminal),
            )
            .await;
        assert!(!res.is_error);
        assert!(ui.seen.lock().unwrap().is_empty(), "yolo must not prompt");
    }

    #[tokio::test]
    async fn reads_do_not_prompt_in_ask_mode() {
        let ui = ScriptedUi::new(vec![]);
        let g = gate(SessionPolicy::new(PermissionMode::Ask), ui.clone());
        let res = g
            .handle(
                &call("read_file", serde_json::json!({"path": "a.txt"})),
                &ctx(),
                None,
                Arc::new(Terminal),
            )
            .await;
        assert!(!res.is_error);
        assert!(ui.seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_run_raises_no_dialog() {
        let ui = ScriptedUi::new(vec![UiResponse::value(ALLOW_ONCE)]);
        let g = gate(SessionPolicy::new(PermissionMode::Manual), ui.clone());
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
        assert!(
            ui.seen.lock().unwrap().is_empty(),
            "must not prompt a dead run"
        );
    }

    #[tokio::test]
    async fn parallel_calls_are_prompted_one_at_a_time() {
        // Two calls in one batch, answered in order; if the gate did not
        // serialise, the answers could cross and the assertions below would be
        // racy rather than deterministic.
        let ui = ScriptedUi::new(vec![
            UiResponse::value(ALLOW_ONCE),
            UiResponse::value(ALLOW_ONCE),
        ]);
        let g = gate(SessionPolicy::new(PermissionMode::Ask), ui.clone());

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
        assert_eq!(ui.seen.lock().unwrap().len(), 2);
    }
}
