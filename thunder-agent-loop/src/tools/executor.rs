use crate::tools::guard::Guard;
use crate::tools::middleware::{ToolMiddleware, ToolPipeline};
use crate::tools::notice::SystemNotice;
use crate::tools::registry::ToolRegistry;
use crate::tools::trace::{GuardHit, GuardKind};
use crate::types::gate::{
    ApprovalAnswer, ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest, ALLOW_ALWAYS,
    ALLOW_ONCE, DENY, DENY_WITH_REASON,
};
use crate::types::message::ToolCall;
use crate::types::policy::{Caller, ToolEffect};
use crate::types::scope::{Scope, ToolScope};
use crate::types::tool::{ToolEventSink, ToolExecutionContext, ToolExecutionResult};
use crate::types::ui::{HostUi, NullHostUi, UiRequest, UiSource};
use futures_util::future::join_all;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ExecutedToolResult {
    pub tool_call: ToolCall,
    pub result: ToolExecutionResult,
}

/// Default wait for a human answer before failing closed (10 minutes, giving
/// the user time to review a diff or a command).
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct ToolExecutor {
    registry: ToolRegistry,
    pipeline: ToolPipeline,
    /// Per-run event sink, installed by the loop before it executes anything.
    ///
    /// Held behind a lock rather than cloned into each call because the loop
    /// sets it once per run, after this executor was built, and a run has
    /// exactly one sink for its whole lifetime.
    event_sink: Arc<Mutex<Option<Arc<dyn ToolEventSink>>>>,
    /// The host's answer to "where does this call land".
    ///
    /// Held the same way as the sink, and for the same reason: the host installs
    /// it once per run, after the executor exists, and every clone must see it.
    /// `None` — no host, or a host with no working area to speak of — means every
    /// call is [`Scope::Unknown`], which is what the headless runtime relies on.
    tool_scope: Arc<Mutex<Option<Arc<dyn ToolScope>>>>,
    /// The host's judge: what a call that cleared the guard still needs before it
    /// may run.
    ///
    /// `None` — no gate installed — means every such call runs. That is the
    /// contract the headless runtime and embedders without a policy have always
    /// relied on, so a kernel that cannot ask a host for a gate is not a kernel
    /// that refuses everything; it is one that approves everything the guard
    /// allowed. Hosts that want asking install a gate.
    tool_gate: Arc<Mutex<Option<Arc<dyn ToolGate>>>>,
    /// The panel dialogs are rendered through. `None` degrades to
    /// [`NullHostUi`], which answers nothing — an `Ask` then refuses, which is
    /// what fail-closed means for a host that cannot ask.
    host_ui: Arc<Mutex<Option<Arc<dyn HostUi>>>>,
    /// Serialises dialogs. `execute_all` runs a batch in parallel, and two
    /// overlapping prompts is how the wrong one gets approved.
    ///
    /// Shared by every clone of this executor (a rebuild creates a new one), so
    /// the calls of one batch queue behind each other.
    prompt_lock: Arc<tokio::sync::Mutex<()>>,
    /// How long to wait for a human before refusing.
    approval_timeout: Duration,
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        let pipeline = ToolPipeline::from_registry(registry.clone());
        Self {
            registry,
            pipeline,
            event_sink: Arc::new(Mutex::new(None)),
            tool_scope: Arc::new(Mutex::new(None)),
            tool_gate: Arc::new(Mutex::new(None)),
            host_ui: Arc::new(Mutex::new(None)),
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }

    pub fn with_pipeline(registry: ToolRegistry, pipeline: ToolPipeline) -> Self {
        Self {
            registry,
            pipeline,
            event_sink: Arc::new(Mutex::new(None)),
            tool_scope: Arc::new(Mutex::new(None)),
            tool_gate: Arc::new(Mutex::new(None)),
            host_ui: Arc::new(Mutex::new(None)),
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }

    /// Install the host's scope classification.
    ///
    /// Consuming, like [`ToolExecutor::with_event_sink`], so a scope cannot leak
    /// from one run into the next through a clone.
    pub fn with_tool_scope(mut self, scope: Arc<dyn ToolScope>) -> Self {
        self.tool_scope = Arc::new(Mutex::new(Some(scope)));
        self
    }

    /// Install the host's judge. Passing `None` removes one, which is what
    /// "uninstall the mode plugin" means in practice.
    pub fn with_tool_gate(mut self, gate: Option<Arc<dyn ToolGate>>) -> Self {
        self.tool_gate = Arc::new(Mutex::new(gate));
        self
    }

    /// Install the panel dialogs are rendered through.
    pub fn with_host_ui(mut self, ui: Arc<dyn HostUi>) -> Self {
        self.host_ui = Arc::new(Mutex::new(Some(ui)));
        self
    }

    /// Override how long to wait for a human answer.
    pub fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// Where this call lands, as the host sees it.
    fn scope_of(&self, tool: &str, effect: ToolEffect, arguments: &serde_json::Value) -> Scope {
        match self.tool_scope.lock().as_ref() {
            Some(scope) => scope.scope_of(tool, effect, arguments),
            None => Scope::Unknown,
        }
    }

    /// The gate, if the host installed one.
    fn gate(&self) -> Option<Arc<dyn ToolGate>> {
        self.tool_gate.lock().clone()
    }

    /// The panel, or the one that answers nothing.
    fn ui(&self) -> Arc<dyn HostUi> {
        self.host_ui
            .lock()
            .clone()
            .unwrap_or_else(|| Arc::new(NullHostUi))
    }

    /// Return this executor with the run's event sink installed.
    ///
    /// Consuming builder, deliberately: the sink belongs to one run, and a
    /// shared `Arc` would leak that run's event sender into every future clone
    /// of this executor — keeping a finished run's event stream open forever.
    /// Swapping the slot on a fresh clone keeps the ownership honest.
    pub fn with_event_sink(mut self, sink: Arc<dyn ToolEventSink>) -> Self {
        self.event_sink = Arc::new(Mutex::new(Some(sink)));
        self
    }

    /// What this call can do to the world: the tool's own declaration when the
    /// tool is registered, otherwise the conservative name-based fallback.
    fn resolve_effect(&self, tool_name: &str) -> crate::types::policy::ToolEffect {
        self.registry
            .get(tool_name)
            .map(|t| t.effect())
            .unwrap_or_else(|| crate::types::policy::ToolEffect::of(tool_name))
    }

    pub fn with_middleware(mut self, mw: Arc<dyn ToolMiddleware>) -> Self {
        self.pipeline.add_middleware(mw);
        self
    }

    pub fn add_middleware(&mut self, mw: Arc<dyn ToolMiddleware>) {
        self.pipeline.add_middleware(mw);
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn pipeline(&self) -> &ToolPipeline {
        &self.pipeline
    }

    /// Run one call through everything a call must pass.
    ///
    /// The order is the whole design, and every entry point funnels here:
    ///
    /// 1. **cancellation** — a call whose run was cancelled must not start;
    /// 2. **the guard** — the refusals no mode, rule or dialog can reach. It
    ///    runs before the gate so nothing below it — not a dialog, not a tool —
    ///    ever sees a call it would have refused;
    /// 3. **the gate** — the host's judge, consulted once per call and never
    ///    cached. No gate installed means allow;
    /// 4. **the ask** — a gate that returns [`GateVerdict::Ask`] stops there.
    ///    This method renders the dialog: serialised, with a host that cannot
    ///    answer treated as a refusal, and the answer reported back to the gate
    ///    so it can remember what the user agreed to;
    /// 5. **the tool** — only now, through the pipeline.
    ///
    /// Arguments that do not parse are not a way round the guard: the registry
    /// parses the same string and errors out before any tool sees them. An
    /// unparseable call reaches the gate as `null` arguments, so a rule keyed on
    /// an argument cannot match it by accident.
    pub async fn run_tool(
        &self,
        call: &ToolCall,
        mut ctx: ToolExecutionContext,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        // Stamp the tool's declared effect and the run's event sink. Both are
        // the executor's to know, not the caller's: a plugin that builds a
        // context by hand should still be judged by what the tool *is*, and
        // should still be able to report events.
        let effect = self.resolve_effect(&call.function.name);
        ctx.effect = Some(effect);
        if ctx.event_sink.is_none() {
            ctx.event_sink = self.event_sink.lock().clone();
        }

        let started = Instant::now();

        if ctx.cancellation_token.is_cancelled() {
            return cancelled_refusal(started);
        }

        let arguments: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
        let scope = self.scope_of(&call.function.name, effect, &arguments);

        if let Some(hit) = Guard::check(&call.function.name, &arguments, effect, scope) {
            return guard_refusal(&hit, started);
        }

        // A gate may only tighten. It chooses between allow, ask and deny for a
        // call the guard has already permitted; it can never loosen the guard,
        // because the guard has already run. With no gate, the call runs — which
        // is what a headless embedder means by installing no policy.
        let caller = match ctx.caller.as_deref() {
            Some(id) => Caller::Plugin(id.to_string()),
            None => Caller::Model,
        };
        let gate = self.gate();
        let request = ToolGateRequest {
            tool: &call.function.name,
            arguments: &arguments,
            effect,
            scope,
            caller: caller.clone(),
            tool_call_id: &call.id,
        };

        let decision = gate.as_ref().map(|gate| (gate, gate.decide(&request)));

        match decision {
            None => self.pipeline.execute(call, &ctx, custom_timeout).await,
            Some((_, GateVerdict::Allow)) | Some((_, GateVerdict::AllowByRule { .. })) => {
                self.pipeline.execute(call, &ctx, custom_timeout).await
            }
            Some((gate, GateVerdict::Deny { reason })) => denied_by_gate(
                gate.label(),
                &call.function.name,
                &crate::types::policy::describe_call(&arguments),
                &reason,
                caller.plugin_id(),
                started,
            ),
            Some((gate, GateVerdict::Ask(prompt))) => {
                let answer = self.ask(gate, &request, &prompt, &ctx).await;
                match answer {
                    AskOutcome::Proceed => self.pipeline.execute(call, &ctx, custom_timeout).await,
                    AskOutcome::Refuse(reason) => denied_by_gate(
                        gate.label(),
                        &call.function.name,
                        &crate::types::policy::describe_call(&arguments),
                        &reason,
                        caller.plugin_id(),
                        started,
                    ),
                }
            }
        }
    }

    /// Render the dialog a gate asked for, and interpret the answer.
    ///
    /// Every way this can end without a human saying "yes" is a refusal — no
    /// panel, a timeout, a dismissal, a value outside the offered set. Silence
    /// is never consent.
    async fn ask(
        &self,
        gate: &Arc<dyn ToolGate>,
        request: &ToolGateRequest<'_>,
        prompt: &ApprovalPrompt,
        ctx: &ToolExecutionContext,
    ) -> AskOutcome {
        // Serialised, and raced against cancellation so a cancelled run releases
        // the lock instead of holding the rest of the batch hostage.
        let _serialised = tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                return AskOutcome::Refuse("the task was cancelled".to_string());
            }
            guard = self.prompt_lock.lock() => guard,
        };

        // Name the caller when it is not the model. A plugin acting on its own
        // initiative must not be presentable as something the assistant asked
        // for — that is the whole difference between an informed yes and a
        // rubber stamp.
        let title = match request.caller.plugin_id() {
            Some(id) if !id.is_empty() => format!("Plugin {id} requests: {}", prompt.title),
            _ => prompt.title.clone(),
        };

        // "Always allow" is offered only when the gate says this call can be
        // narrowed to a rule it is willing to keep. The kernel does not derive
        // rules — remembering one is the gate's job, and so is deciding whether
        // it can be done safely for this call.
        let options: Vec<String> = if prompt.allow_always {
            vec![
                ALLOW_ONCE.to_string(),
                ALLOW_ALWAYS.to_string(),
                DENY.to_string(),
                DENY_WITH_REASON.to_string(),
            ]
        } else {
            vec![
                ALLOW_ONCE.to_string(),
                DENY.to_string(),
                DENY_WITH_REASON.to_string(),
            ]
        };

        let timeout_ms = Some(self.approval_timeout.as_millis() as u64);
        let response = self
            .ui()
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
            None => return AskOutcome::Refuse("no answer (timed out or dismissed)".to_string()),
        };

        match choice.as_str() {
            ALLOW_ONCE => AskOutcome::Proceed,
            ALLOW_ALWAYS if prompt.allow_always => {
                gate.record_answer(request, &ApprovalAnswer::AllowAlways);
                AskOutcome::Proceed
            }
            // Cannot happen: the option is not offered when `allow_always` is
            // false. Fail closed anyway rather than trust the renderer.
            ALLOW_ALWAYS => {
                AskOutcome::Refuse("no safe 'always allow' rule exists for this call".to_string())
            }
            DENY => {
                gate.record_answer(request, &ApprovalAnswer::Deny);
                AskOutcome::Refuse("the user declined".to_string())
            }
            DENY_WITH_REASON => {
                let reason = self
                    .ui()
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
                    gate.record_answer(request, &ApprovalAnswer::Deny);
                    AskOutcome::Refuse("the user declined without giving a reason".to_string())
                } else {
                    let reason = reason.trim().to_string();
                    gate.record_answer(
                        request,
                        &ApprovalAnswer::DenyWithReason {
                            reason: reason.clone(),
                        },
                    );
                    AskOutcome::Refuse(format!("the user declined: {reason}"))
                }
            }
            // An answer outside the offered set is a refusal: a surprising value
            // must never widen access.
            _ => AskOutcome::Refuse(format!("unrecognised answer: {choice}")),
        }
    }

    /// Execute a single call through the full pipeline.
    ///
    /// Same funnel as [`ToolExecutor::execute_all`], minus the batch — which is
    /// exactly why a plugin can use this without becoming a way around it.
    pub async fn execute_one(
        &self,
        call: &ToolCall,
        turn: usize,
        cancellation_token: CancellationToken,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        let ctx = ToolExecutionContext {
            tool_call_id: call.id.clone(),
            turn,
            cancellation_token,
            ..Default::default()
        };
        self.run_tool(call, ctx, custom_timeout).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_all(
        &self,
        tool_calls: &[ToolCall],
        turn: usize,
        cancellation_token: CancellationToken,
        route: Option<String>,
        custom_timeout: Option<Duration>,
    ) -> Vec<ExecutedToolResult> {
        if tool_calls.is_empty() {
            return Vec::new();
        }

        // Shared across the parallel futures, so each one clones the string
        // rather than moving it out of the closure.
        let route = route.map(Arc::new);
        let futures = tool_calls.iter().map(|tc| {
            let route = route.clone();
            let tc_clone = tc.clone();
            let token = cancellation_token.clone();

            async move {
                let ctx = ToolExecutionContext {
                    tool_call_id: tc_clone.id.clone(),
                    turn,
                    cancellation_token: token,
                    route: route.as_ref().map(|r| r.as_str().to_string()),
                    ..Default::default()
                };
                // Route through the funnel so every parallel call is stamped,
                // judged and recorded the same way.
                let res = self.run_tool(&tc_clone, ctx, custom_timeout).await;
                ExecutedToolResult {
                    tool_call: tc_clone,
                    result: res,
                }
            }
        });

        join_all(futures).await
    }
}

/// What a dialog came to.
enum AskOutcome {
    Proceed,
    Refuse(String),
}

/// The refusal a cancelled call gets: nothing ran, and saying so is the point.
fn cancelled_refusal(started: Instant) -> ToolExecutionResult {
    let notice = SystemNotice::new(
        "Guard",
        "Tool execution cancelled before launch",
        "Invocation halted immediately by the cancellation signal; nothing ran.",
    )
    .with_guidance("You may safely formulate a new prompt or retry.");
    ToolExecutionResult::error(
        "Tool execution cancelled by user signal".to_string(),
        started.elapsed(),
    )
    .with_telemetry(notice)
}

/// The refusal a guard hit gets, written for the model that will read it.
///
/// The guidance has to be actionable, because the model's next move depends on
/// it: a refusal that reads as "try again, differently" produces a retry loop,
/// and this is exactly the shape of refusal the model cannot retry past.
fn guard_refusal(hit: &GuardHit, started: Instant) -> ToolExecutionResult {
    let (action, reason, ground_truth, guidance) = match hit.kind {
        GuardKind::DangerousCommand => (
            "Dangerous command refused",
            format!(
                "Refused: this command matches '{}'. The agent does not run it at any permission \
                 level or in any mode, and changing the mode will not change that.",
                hit.pattern
            ),
            "The command was intercepted before reaching the shell; nothing ran.",
            "If it is genuinely needed, ask the user to run it themselves. Otherwise take a \
             different approach.",
        ),
        GuardKind::CriticalPath => (
            "Critical path refused",
            format!(
                "Refused: '{}' is off limits — protected regardless of mode or approval.",
                hit.detail
            ),
            "Nothing was read or written.",
            "Choose another target. Asking the user to approve this will not help.",
        ),
    };

    let notice = SystemNotice::new("Guard", action, ground_truth).with_guidance(guidance);
    ToolExecutionResult::error(reason, started.elapsed()).with_telemetry(notice)
}

/// The refusal a gate denial or an unanswered dialog gets.
///
/// Same shape as it has always been — "was not approved", the detail of what
/// would have run, the reason in the user's words when they gave one — because
/// it is written for the same reader: a model deciding what to do next, which
/// is to stop and ask, not to hunt for a different tool that achieves the same
/// effect.
fn denied_by_gate(
    gate: &str,
    tool: &str,
    detail: &str,
    reason: &str,
    caller: Option<&str>,
    started: Instant,
) -> ToolExecutionResult {
    let mut message = format!("Error: '{tool}' was not approved");
    if let Some(id) = caller.filter(|c| !c.is_empty()) {
        message.push_str(&format!(" (requested by plugin '{id}')"));
    }
    if !detail.is_empty() {
        message.push_str(&format!(" — {detail}"));
    }
    message.push_str(&format!(". Reason: {reason}"));

    let guidance = format!(
        "The user declined this specific call ({reason}). Do not retry it unchanged and do not \
         look for another tool that achieves the same effect — ask what they want instead."
    );

    let notice = SystemNotice::new(
        gate,
        format!("Blocked '{tool}' — the user did not approve it"),
        "The call never ran. The workspace is unchanged.",
    )
    .with_guidance(guidance);

    ToolExecutionResult::error(message, started.elapsed()).with_telemetry(notice)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two refusals that carry structured telemetry name their layer, so a
    /// reader of the timeline can tell a guard refusal from a gate one.
    #[test]
    fn refusals_are_attributed_to_their_layer() {
        let guard = guard_refusal(
            &GuardHit {
                kind: GuardKind::DangerousCommand,
                pattern: "rm -rf /".into(),
                detail: "rm -rf /".into(),
            },
            Instant::now(),
        );
        assert_eq!(guard.telemetry.as_ref().unwrap().layer, "Guard");

        let gate = denied_by_gate(
            "mode",
            "bash",
            "ls",
            "the user declined",
            None,
            Instant::now(),
        );
        assert_eq!(gate.telemetry.as_ref().unwrap().layer, "mode");
        assert!(gate.output.contains("was not approved"));
    }
}
