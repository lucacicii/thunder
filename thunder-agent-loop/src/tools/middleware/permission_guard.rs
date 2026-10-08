use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::config::Permission;
use crate::types::message::ToolCall;
use crate::types::policy::{
    describe_call, AllowRule, ApprovalRequest, Caller, PermissionMode, SessionPolicy, Verdict,
    ALLOW_ALWAYS, ALLOW_ONCE, DENY, DENY_WITH_REASON,
};
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use crate::types::ui::{HostUi, UiRequest, UiSource};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Permission Guard Middleware.
///
/// Second line of defense behind the host's tool-registration gate. The host
/// decides which built-in tools exist at all; this layer independently refuses
/// any privileged call that reaches execution anyway — e.g. a tool re-added by a
/// custom host path, or a nested call issued from another tool.
///
/// Rejection is reported with structured telemetry so the model learns the
/// ground truth ("the file was not written") instead of assuming success.
/// The one layer that judges a tool call.
///
/// Merges what used to be two adjacent middlewares:
///
/// * `PermissionGuardMiddleware` — refused what the role's tier forbids;
/// * `ApprovalGate` — asked a human about the rest.
///
/// They were two layers doing one job with two rule sets, and the seam between
/// them was load-bearing in a way nothing tested as a unit: the guard had to
/// stay outermost (or a user could approve past the tier) and the gate had to
/// stay outside `TransactionMiddleware` (or a refused call would stage files).
/// Keeping the outer name and folding the gate in makes the layer a single
/// thing with a single contract.
///
/// # Contract
///
/// 1. Ask [`SessionPolicy::decide`] — it is the only judge.
/// 2. `Deny` → a tool result carrying a `SystemNotice`. Not an exception: the
///    model is told nothing happened, so it stops retrying.
/// 3. `Ask` → raise a dialog, then act on the answer.
/// 4. Every non-answer is a refusal: no UI, timeout, dismissal or a cancelled
///    run all land on the deny branch. This layer never fails open.
/// 5. Dialogs are serialised. `ToolExecutor::execute_all` runs a batch in
///    parallel, and three overlapping prompts is how the wrong one gets
///    approved.
#[derive(Clone)]
pub struct PermissionGuardMiddleware {
    policy: Arc<SessionPolicy>,
    /// Panel for dialogs. `None` means every `Ask` resolves as cancelled.
    ui: Arc<dyn HostUi>,
    /// Serialises dialogs; see contract point 5.
    prompt_lock: Arc<tokio::sync::Mutex<()>>,
    /// How long to wait for a human before refusing.
    timeout: Duration,
    /// Allowed workspace roots, surfaced in refusals so the model learns its
    /// legal targets instead of probing with other tools.
    workspace_roots: Vec<PathBuf>,
    /// Whether this run is read-only, captured at construction because
    /// `refuse` is synchronous and the policy is behind a lock.
    tier_hint: TierHint,
}

/// A snapshot of the parts of the policy that synchronous code needs.
#[derive(Clone, Copy, Default)]
struct TierHint {
    read_only: bool,
}

impl PermissionGuardMiddleware {
    /// Layer name, and the anchor the pipeline inserts behind.
    pub const NAME: &'static str = "PermissionGuardMiddleware";

    /// Default wait for human approval before failing closed (10 minutes, giving the developer time to review diffs).
    pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

    pub fn new(policy: Arc<SessionPolicy>, ui: Arc<dyn HostUi>) -> Self {
        Self {
            policy,
            ui,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            timeout: Self::DEFAULT_APPROVAL_TIMEOUT,
            workspace_roots: Vec::new(),
            tier_hint: TierHint { read_only: false },
        }
    }

    /// Override how long to wait for a human answer.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// A guard with no policy behind it: it enforces only the static tier.
    ///
    /// For embedders that have not adopted [`SessionPolicy`]. The resulting
    /// pipeline has no notion of modes or remembered rules, so every call the
    /// tier permits runs without asking — which is the pre-policy behaviour,
    /// stated plainly rather than approximated.
    pub fn new_tier_only(tier: Permission, workspace_roots: Vec<PathBuf>) -> Self {
        let policy = SessionPolicy::new(tier, PermissionMode::default());
        Self {
            policy,
            ui: Arc::new(crate::types::ui::NullHostUi),
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            timeout: Self::DEFAULT_APPROVAL_TIMEOUT,
            tier_hint: TierHint {
                read_only: tier == Permission::Read,
            },
            workspace_roots,
        }
    }

    /// Attach the jail's allowed roots for rejection guidance.
    pub fn with_workspace_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.workspace_roots = roots;
        self
    }

    pub fn policy(&self) -> &Arc<SessionPolicy> {
        &self.policy
    }

    fn roots_display(&self) -> String {
        self.workspace_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn roots_hint(&self) -> String {
        if self.workspace_roots.is_empty() {
            String::new()
        } else {
            format!(
                " Reads inside the allowed workspace roots are still available: [{}].",
                self.roots_display()
            )
        }
    }
}

#[async_trait]
impl ToolMiddleware for PermissionGuardMiddleware {
    fn name(&self) -> &str {
        Self::NAME
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();
        let tool = call.function.name.as_str();

        // A cancelled run must not raise a dialog nobody will answer.
        if ctx.cancellation_token.is_cancelled() {
            return ToolExecutionResult::error(
                format!("Error: '{tool}' was not run because the task was cancelled"),
                start.elapsed(),
            );
        }

        let caller = match &ctx.caller {
            Some(id) => Caller::Plugin(id.clone()),
            None => Caller::Model,
        };

        // Judge the tool's *declared* effect, which the executor stamped onto
        // the context. Falling back to the name heuristic keeps a hand-built
        // context (a plugin invoking a tool directly) no weaker than before.
        let effect = ctx
            .effect
            .unwrap_or_else(|| crate::types::policy::ToolEffect::of(tool));
        let verdict = self.policy.decide_with_effect(call, effect, &caller).await;

        let request = match verdict {
            Verdict::Allow => return next.handle(call, ctx, timeout).await,
            Verdict::Deny { reason } => {
                let detail = describe_call(&parsed_args(call));
                return self.refuse(tool, &detail, &reason, caller.plugin_id(), start);
            }
            Verdict::Ask(request) => request,
        };

        match self.prompt(&request, call, ctx).await {
            PromptOutcome::AllowOnce => next.handle(call, ctx, timeout).await,
            PromptOutcome::AllowAlways(rule) => {
                if let Some(rule) = rule {
                    self.policy.remember(rule).await;
                }
                next.handle(call, ctx, timeout).await
            }
            PromptOutcome::Refuse(reason) => {
                let detail = describe_call(&parsed_args(call));
                self.refuse(tool, &detail, &reason, caller.plugin_id(), start)
            }
        }
    }
}

/// The call's arguments as a value.
///
/// A provider sends them as a JSON string, and an unparseable one must not
/// become an empty object that quietly matches a narrow "always allow" rule.
fn parsed_args(call: &ToolCall) -> serde_json::Value {
    serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null)
}

enum PromptOutcome {
    AllowOnce,
    AllowAlways(Option<AllowRule>),
    Refuse(String),
}

impl PermissionGuardMiddleware {
    /// Raise the dialog and interpret the answer.
    async fn prompt(
        &self,
        request: &ApprovalRequest,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
    ) -> PromptOutcome {
        // Serialised, and raced against cancellation so a cancelled run releases
        // the lock instead of holding the rest of the batch hostage.
        let _serialised = tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                return PromptOutcome::Refuse("the task was cancelled".to_string());
            }
            guard = self.prompt_lock.lock() => guard,
        };

        let args = parsed_args(call);
        // `ALWAYS` is offered only when a narrow rule can be derived. A blanket
        // "always allow this tool" is not something this design grants —
        // notably for plugin tools, whose arguments carry no scoping the host
        // can reason about.
        let rule = AllowRule::for_call(&request.tool, &args);
        let options: Vec<String> = request
            .options
            .iter()
            .filter(|o| o.as_str() != ALLOW_ALWAYS || rule.is_some())
            .cloned()
            .collect();

        // Name the caller when it is not the model. A plugin acting on its own
        // initiative must not be presentable as something the assistant asked
        // for — that is the whole difference between an informed yes and a
        // rubber stamp.
        let title = match request.caller.plugin_id() {
            Some(id) if !id.is_empty() => format!("Plugin {id} requests: {}", request.title),
            _ => request.title.clone(),
        };

        let timeout_ms = Some(self.timeout.as_millis() as u64);
        let response = self
            .ui
            .request(
                UiSource::Host,
                UiRequest::Select {
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
                        UiRequest::Input {
                            title: "Reason for denying (the agent will be told)".to_string(),
                            placeholder: Some("e.g. leave the production config alone".to_string()),
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

    /// Refusal result, phrased for the model.
    fn refuse(
        &self,
        tool: &str,
        detail: &str,
        reason: &str,
        caller: Option<&str>,
        start: Instant,
    ) -> ToolExecutionResult {
        let mut message = format!("Error: '{tool}' was not approved");
        if let Some(id) = caller.filter(|c| !c.is_empty()) {
            message.push_str(&format!(" (requested by plugin '{id}')"));
        }
        if !detail.is_empty() {
            message.push_str(&format!(" — {detail}"));
        }
        message.push_str(&format!(". Reason: {reason}"));

        let read_only = self.tier_hint.read_only;
        let mut guidance = format!(
            "The user declined this specific call{}. Do not retry it unchanged and do not look for \
             another tool that achieves the same effect — ask what they want instead.{}",
            if reason.is_empty() {
                String::new()
            } else {
                format!(" ({reason})")
            },
            self.roots_hint()
        );
        if read_only {
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
