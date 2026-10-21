use crate::gate::{
    ApprovalAnswer, ApprovalPrompt, GateVerdict, ToolGate, ToolGateRequest, ALLOW_ALWAYS,
    ALLOW_ONCE, DENY, DENY_WITH_REASON,
};
use crate::guard::Guard;
use crate::plugin::{CallToolResult, InterceptorPlugin, PreExecutionVerdict, SubToolInvoker};
use crate::registry::ToolRegistry;
use crate::tool_policy::ToolExecutionContext;
use crate::trace::{
    redact_arguments, withhold, AskAnswer, Decision, DenyBy, GuardHit, GuardKind, ToolTrace,
    ToolTraceSink, TraceResult, TraceTiming, TRACE_SCHEMA,
};
use crate::ui::{HostUi, NullHostUi, UiRequest, UiSource};
use async_trait::async_trait;
use futures_util::future::join_all;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thunder_agent_types::message::ToolCall;
use thunder_agent_types::notice::SystemNotice;
use thunder_agent_types::policy::{Caller, ToolEffect};
use thunder_agent_types::scope::{Scope, ToolScope};
use thunder_agent_types::tool::{ToolEventSink, ToolExecutionResult};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ExecutedToolResult {
    pub tool_call: ToolCall,
    pub result: ToolExecutionResult,
}

/// Default wait for a human answer before failing closed (10 minutes, giving
/// the user time to review a diff or a command).
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// How much of a tool's output the trace keeps. The full text is already on
/// disk where the tool put it; a record that embeds every megabyte is a record
/// nobody reads.
const OUTPUT_PREVIEW_CHARS: usize = 2048;

#[derive(Clone)]
pub struct ToolExecutor {
    registry: ToolRegistry,
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
    /// Where finished traces go, and what to stamp them with.
    ///
    /// Installed per run by the loop (which is the only thing that knows the
    /// session and the task), so a trace never has to guess its own identity.
    trace: Arc<Mutex<TraceSlot>>,
    /// Interceptors executed before and after every tool call.
    interceptors: Arc<Mutex<Vec<Arc<dyn InterceptorPlugin>>>>,
}

/// The trace output side: a sink, and the run's identity.
///
/// `sink` is `None` when a host installed none, which is not a reason to stop
/// recording — the loop's own per-run sink is always there, so a call that ran
/// inside a run is always traced.
#[derive(Clone, Default)]
struct TraceSlot {
    sink: Option<Arc<dyn ToolTraceSink>>,
    session_id: Option<String>,
    task_id: Option<String>,
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        Self {
            registry,
            event_sink: Arc::new(Mutex::new(None)),
            tool_scope: Arc::new(Mutex::new(None)),
            tool_gate: Arc::new(Mutex::new(None)),
            host_ui: Arc::new(Mutex::new(None)),
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            trace: Arc::new(Mutex::new(TraceSlot::default())),
            interceptors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Add an interceptor plugin into the executor pipeline.
    pub fn with_interceptor(self, interceptor: Arc<dyn InterceptorPlugin>) -> Self {
        self.interceptors.lock().push(interceptor);
        self
    }

    /// Add an interceptor to the shared pipeline in-place.
    pub fn add_interceptor(&self, interceptor: Arc<dyn InterceptorPlugin>) {
        self.interceptors.lock().push(interceptor);
    }

    /// Install the sink finished traces go to.
    ///
    /// Consuming, like the event sink: the trace output belongs to one run, and
    /// a shared slot would keep a finished run's sink alive for every later
    /// clone.
    pub fn with_trace_sink(mut self, sink: Option<Arc<dyn ToolTraceSink>>) -> Self {
        let mut slot = self.trace.lock().clone();
        slot.sink = sink;
        self.trace = Arc::new(Mutex::new(slot));
        self
    }

    /// Install the sink a run's traces go to, on the slot every clone shares.
    ///
    /// In place rather than consuming, unlike [`ToolExecutor::with_trace_sink`].
    /// The difference is what the caller needs: the loop's plugin invoker holds
    /// a clone taken before the run existed, and a consuming builder would give
    /// the run's own calls a sink the invoker's calls still could not see — so a
    /// plugin-initiated call would be the one call in the system with no record.
    /// The loop clears this again when the run settles, so a finished run's
    /// event sender does not stay alive in a slot later clones share.
    pub fn install_run_trace(&self, sink: Arc<dyn ToolTraceSink>) {
        self.trace.lock().sink = Some(sink);
    }

    /// Forget the run's sink. Called when the run settles.
    pub fn clear_run_trace(&self) {
        self.trace.lock().sink = None;
    }

    /// Stamp this executor's traces with the run they belong to.
    pub fn with_trace_ids(mut self, session_id: Option<String>, task_id: Option<String>) -> Self {
        let mut slot = self.trace.lock().clone();
        slot.session_id = session_id;
        slot.task_id = task_id;
        self.trace = Arc::new(Mutex::new(slot));
        self
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
    fn resolve_effect(&self, tool_name: &str) -> thunder_agent_types::policy::ToolEffect {
        self.registry
            .get(tool_name)
            .map(|t| t.effect())
            .unwrap_or_else(|| thunder_agent_types::policy::ToolEffect::of(tool_name))
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
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
    /// 5. **the tool** — only now, and only through the registry.
    ///
    /// Arguments that do not parse are not a way round the guard: the registry
    /// parses the same string and errors out before any tool sees them. An
    /// unparseable call reaches the gate as `null` arguments, so a rule keyed on
    /// an argument cannot match it by accident.
    ///
    /// # The one exit
    ///
    /// This method has no early `return` and exactly one place that finishes a
    /// trace: the work happens in [`ToolExecutor::dispatch`], which returns a
    /// result *to* here, and the record is closed and published on the way out.
    /// That is what makes "every call leaves exactly one trace" structural
    /// rather than a promise — there is no path out of this function that skips
    /// the record, because no path out of it exists at all.
    pub async fn run_tool(
        &self,
        call: &ToolCall,
        mut ctx: ToolExecutionContext,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        // Stamp the tool's declared effect, the run's event sink and the panel.
        // All three are the executor's to know, not the caller's: a plugin that
        // builds a context by hand should still be judged by what the tool *is*,
        // should still be able to report events, and should still be able to
        // ask the user something.
        let effect = self.resolve_effect(&call.function.name);
        ctx.effect = Some(effect);
        if ctx.event_sink.is_none() {
            ctx.event_sink = self.event_sink.lock().clone();
        }
        if ctx.ui.is_none() {
            ctx.ui = Some(crate::ui::UiHandle(self.ui()));
        }
        if ctx.sub_invoker.is_none() {
            ctx.sub_invoker = Some(Arc::new(ExecutorSubToolInvoker::new(self.clone())));
        }
        if ctx.call_chain.is_empty() {
            ctx.call_chain.push(call.function.name.clone());
        }

        let started = Instant::now();
        let mut draft = TraceDraft::begin(call, &ctx, effect, started);

        let result = self.dispatch(call, &ctx, custom_timeout, &mut draft).await;

        let ids = {
            let slot = self.trace.lock();
            (slot.session_id.clone(), slot.task_id.clone())
        };
        let trace = draft.finish(&result, started, ids);
        self.publish(&trace);
        result
    }

    /// Do everything the funnel does, recording each verdict on the way.
    ///
    /// Split from [`ToolExecutor::run_tool`] precisely so the refusal paths can
    /// return early *here*, where returning early cannot skip the record.
    async fn dispatch(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        custom_timeout: Option<Duration>,
        draft: &mut TraceDraft,
    ) -> ToolExecutionResult {
        let started = draft.started;

        if ctx.cancellation_token.is_cancelled() {
            draft.decision = Decision::Cancelled;
            return cancelled_refusal(started);
        }

        let arguments: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
        // The record keeps a redacted copy; the tool keeps the original. A
        // trace is a durable artifact, and a credential that reached the trace
        // has reached a file the session's author may never read.
        draft.arguments = arguments.clone();
        let redactions = redact_arguments(&mut draft.arguments);
        draft.redactions = redactions.rules;
        draft.secrets = redactions.secrets;

        let scope = self.scope_of(&call.function.name, draft.effect, &arguments);
        draft.scope = scope;

        if let Some(hit) = Guard::check(&call.function.name, &arguments, draft.effect, scope) {
            draft.decision = Decision::Deny {
                reason: guard_reason(&hit),
                by: DenyBy::Guard,
            };
            draft.guard = Some(hit.clone());
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
            effect: draft.effect,
            scope,
            caller: caller.clone(),
            tool_call_id: &call.id,
        };

        let decision = gate.as_ref().map(|gate| (gate, gate.decide(&request)));
        draft.mode = gate.as_ref().map(|gate| gate.label().to_string());

        let deny = |draft: &mut TraceDraft, reason: &str| {
            draft.decision = Decision::Deny {
                reason: reason.to_string(),
                by: DenyBy::Gate,
            };
        };

        match decision {
            None | Some((_, GateVerdict::Allow)) => {
                draft.decision = Decision::Allow;
                self.execute_tool(call, ctx, custom_timeout, draft).await
            }
            Some((_, GateVerdict::AllowByRule { rule })) => {
                draft.decision = Decision::AllowByRule { rule };
                self.execute_tool(call, ctx, custom_timeout, draft).await
            }
            Some((_, GateVerdict::Deny { reason })) => {
                deny(draft, &reason);
                denied_by_gate(
                    draft.mode.as_deref().unwrap_or("gate"),
                    &call.function.name,
                    &thunder_agent_types::policy::describe_call(&arguments),
                    &reason,
                    caller.plugin_id(),
                    started,
                )
            }
            Some((gate, GateVerdict::Ask(prompt))) => {
                draft.decision = Decision::Asked;
                let answer = self.ask(gate, &request, &prompt, ctx, draft).await;
                match answer {
                    AskOutcome::Proceed(_) => {
                        self.execute_tool(call, ctx, custom_timeout, draft).await
                    }
                    AskOutcome::Refuse { reason, .. } => denied_by_gate(
                        gate.label(),
                        &call.function.name,
                        &thunder_agent_types::policy::describe_call(&arguments),
                        &reason,
                        caller.plugin_id(),
                        started,
                    ),
                }
            }
        }
    }

    /// Run the tool, timing it for the record.
    ///
    /// Straight to the registry: sanitising, spilling oversized output to the
    /// scratchpad, truncating and the timeout all happen inside
    /// [`ToolRegistry::execute_tool_call`], which every call reaches anyway.
    /// There used to be an onion of layers here; a layer to repeat what the
    /// registry already does is a second ceiling nobody could reason about, and
    /// anything that *judges* a call now runs before this point, in
    /// [`ToolExecutor::dispatch`].
    async fn execute_tool(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        custom_timeout: Option<Duration>,
        draft: &mut TraceDraft,
    ) -> ToolExecutionResult {
        let arguments: serde_json::Value =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);

        // Pre-execute interceptor pipeline sorted by priority descending (highest priority runs first)
        let mut interceptors = self.interceptors.lock().clone();
        interceptors.sort_by_key(|a| std::cmp::Reverse(a.priority()));

        for interceptor in &interceptors {
            match interceptor
                .pre_execute(&call.function.name, &arguments, ctx)
                .await
            {
                Ok(PreExecutionVerdict::Allow) => {}
                Ok(PreExecutionVerdict::Deny { reason }) => {
                    draft.decision = Decision::Deny {
                        reason: reason.clone(),
                        by: DenyBy::Gate,
                    };
                    return ToolExecutionResult::error(reason, Duration::from_millis(0));
                }
                Ok(PreExecutionVerdict::AskUser { prompt }) => {
                    let asked_at = Instant::now();
                    let _serialised = tokio::select! {
                        biased;
                        _ = ctx.cancellation_token.cancelled() => {
                            draft.answer = Some(AskAnswer::Timeout);
                            draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
                            let reason = "the task was cancelled".to_string();
                            draft.decision = Decision::Deny {
                                reason: reason.clone(),
                                by: DenyBy::Gate,
                            };
                            return ToolExecutionResult::error(reason, Duration::from_millis(0));
                        }
                        guard = self.prompt_lock.lock() => guard,
                    };
                    if self.host_ui.lock().is_none() {
                        draft.answer = Some(AskAnswer::NoUi);
                        draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
                        let reason = format!("User denied execution (no UI attached): {}", prompt);
                        draft.decision = Decision::Deny {
                            reason: reason.clone(),
                            by: DenyBy::Gate,
                        };
                        return ToolExecutionResult::error(reason, Duration::from_millis(0));
                    }
                    let timeout_ms = Some(self.approval_timeout.as_millis() as u64);
                    let resp = self
                        .ui()
                        .request(
                            UiSource::Host,
                            UiRequest::Select {
                                title: prompt.clone(),
                                // The interceptor prompt already inlines
                                // "title: detail" (see ModePlugin::pre_execute).
                                detail: None,
                                options: vec![ALLOW_ONCE.to_string(), DENY.to_string()],
                                timeout_ms,
                            },
                        )
                        .await;
                    draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
                    match resp.text() {
                        Some(text) if text == ALLOW_ONCE => {
                            draft.answer = Some(AskAnswer::AllowOnce);
                        }
                        _ => {
                            draft.answer = Some(AskAnswer::Denied);
                            let reason = format!("User denied execution: {}", prompt);
                            draft.decision = Decision::Deny {
                                reason: reason.clone(),
                                by: DenyBy::Gate,
                            };
                            return ToolExecutionResult::error(reason, Duration::from_millis(0));
                        }
                    }
                }
                Err(err) => {
                    draft.decision = Decision::Deny {
                        reason: err.clone(),
                        by: DenyBy::Gate,
                    };
                    return ToolExecutionResult::error(err, Duration::from_millis(0));
                }
            }
        }

        let started = Instant::now();
        let mut result = self
            .registry
            .execute_tool_call(call, ctx.clone(), custom_timeout)
            .await;
        draft.timing.exec_ms = started.elapsed().as_millis() as u64;

        // Post-execute interceptor pipeline executed in reverse order (Onion unwinding)
        let mut mcp_result = if result.is_error {
            CallToolResult::error_text(&result.output)
        } else {
            CallToolResult::success_text(&result.output)
        };
        for interceptor in interceptors.iter().rev() {
            interceptor
                .post_execute(&call.function.name, &arguments, &mut mcp_result, ctx)
                .await;
        }

        // If post_execute interceptors modified the content or error status, propagate back
        if !mcp_result.content.is_empty() {
            result.output = mcp_result.plain_text();
        }
        if let Some(is_err) = mcp_result.is_error {
            result.is_error = is_err;
        }

        result
    }

    /// Hand one finished trace to the sink, if a run installed one.
    ///
    /// The kernel never writes a trace itself: it does not know where a store
    /// lives, and a kernel that picked a path would be inventing a host's
    /// policy. It emits, and a host keeps.
    fn publish(&self, trace: &ToolTrace) {
        if let Some(sink) = self.trace.lock().sink.clone() {
            sink.record(trace);
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
        draft: &mut TraceDraft,
    ) -> AskOutcome {
        let asked_at = Instant::now();

        // Serialised, and raced against cancellation so a cancelled run releases
        // the lock instead of holding the rest of the batch hostage.
        let _serialised = tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                draft.answer = Some(AskAnswer::Timeout);
                draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
                return AskOutcome::Refuse {
                    answer: AskAnswer::Timeout,
                    reason: "the task was cancelled".to_string(),
                };
            }
            guard = self.prompt_lock.lock() => guard,
        };

        // No panel at all is a different situation from a panel that did not
        // answer, and the record says which: one is a headless host, the other
        // is a user who walked away. Both refuse.
        if self.host_ui.lock().is_none() {
            draft.answer = Some(AskAnswer::NoUi);
            draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
            return AskOutcome::Refuse {
                answer: AskAnswer::NoUi,
                reason: "no user interface is attached to answer it".to_string(),
            };
        }

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
                    // The substance being approved. A dialog without it asks
                    // for a blind yes; the gate computed this string for
                    // exactly this call.
                    detail: Some(prompt.detail.clone()),
                    options: options.clone(),
                    timeout_ms,
                },
            )
            .await;

        let choice = match response.text() {
            Some(text) => text.to_string(),
            None => {
                draft.answer = Some(AskAnswer::Timeout);
                draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
                return AskOutcome::Refuse {
                    answer: AskAnswer::Timeout,
                    reason: "no answer (timed out or dismissed)".to_string(),
                };
            }
        };

        let outcome = match choice.as_str() {
            ALLOW_ONCE => AskOutcome::Proceed(AskAnswer::AllowOnce),
            ALLOW_ALWAYS if prompt.allow_always => {
                gate.record_answer(request, &ApprovalAnswer::AllowAlways);
                AskOutcome::Proceed(AskAnswer::AllowAlways)
            }
            // Cannot happen: the option is not offered when `allow_always` is
            // false. Fail closed anyway rather than trust the renderer.
            ALLOW_ALWAYS => AskOutcome::Refuse {
                answer: AskAnswer::Unrecognised,
                reason: "no safe 'always allow' rule exists for this call".to_string(),
            },
            DENY => {
                gate.record_answer(request, &ApprovalAnswer::Deny);
                AskOutcome::Refuse {
                    answer: AskAnswer::Denied,
                    reason: "the user declined".to_string(),
                }
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
                    AskOutcome::Refuse {
                        answer: AskAnswer::Denied,
                        reason: "the user declined without giving a reason".to_string(),
                    }
                } else {
                    let reason = reason.trim().to_string();
                    gate.record_answer(
                        request,
                        &ApprovalAnswer::DenyWithReason {
                            reason: reason.clone(),
                        },
                    );
                    AskOutcome::Refuse {
                        answer: AskAnswer::Denied,
                        reason: format!("the user declined: {reason}"),
                    }
                }
            }
            // An answer outside the offered set is a refusal: a surprising value
            // must never widen access.
            _ => AskOutcome::Refuse {
                answer: AskAnswer::Unrecognised,
                reason: format!("unrecognised answer: {choice}"),
            },
        };

        draft.answer = Some(outcome.answer());
        draft.timing.ask_ms = asked_at.elapsed().as_millis() as u64;
        outcome
    }

    /// Execute a single call through the funnel.
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
    /// The user said yes — once, or with a standing rule.
    Proceed(AskAnswer),
    /// Anything else, including silence.
    Refuse { answer: AskAnswer, reason: String },
}

impl AskOutcome {
    /// What the record says the user answered.
    fn answer(&self) -> AskAnswer {
        match self {
            AskOutcome::Proceed(answer) | AskOutcome::Refuse { answer, .. } => *answer,
        }
    }
}

/// The refusal a guard hit is worth putting in the record.
fn guard_reason(hit: &GuardHit) -> String {
    format!(
        "refused by the kernel's guard: this call matches '{}'",
        hit.pattern
    )
}

/// The record under construction, filled in as the funnel makes its decisions.
///
/// Deliberately not the record itself: a [`ToolTrace`] is the finished artifact,
/// and everything here is what is known *before* the call has ended.
struct TraceDraft {
    trace_id: String,
    turn: usize,
    tool_call_id: String,
    tool: String,
    caller: Caller,
    effect: ToolEffect,
    mode: Option<String>,
    scope: Scope,
    arguments: serde_json::Value,
    decision: Decision,
    guard: Option<GuardHit>,
    answer: Option<AskAnswer>,
    redactions: Vec<String>,
    /// Literal values the arguments declared secret, withheld from anything
    /// derived from them.
    secrets: Vec<String>,
    timing: TraceTiming,
    started: Instant,
    started_at_ms: u64,
}

impl TraceDraft {
    fn begin(
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        effect: ToolEffect,
        started: Instant,
    ) -> Self {
        Self {
            trace_id: trace_id(&call.id, started),
            turn: ctx.turn,
            tool_call_id: call.id.clone(),
            tool: call.function.name.clone(),
            caller: match ctx.caller.as_deref() {
                Some(id) => Caller::Plugin(id.to_string()),
                None => Caller::Model,
            },
            effect,
            mode: None,
            scope: Scope::Unknown,
            arguments: serde_json::Value::Null,
            // Overwritten by every path below; a draft that somehow survived
            // would record a call that was allowed, which is the wrong way to
            // fail for a record nobody can re-derive.
            decision: Decision::Allow,
            guard: None,
            answer: None,
            redactions: Vec::new(),
            secrets: Vec::new(),
            timing: TraceTiming::default(),
            started,
            started_at_ms: crate::trace::now_ms(),
        }
    }

    /// Close the record. `result` is what the model was told.
    fn finish(
        mut self,
        result: &ToolExecutionResult,
        started: Instant,
        ids: (Option<String>, Option<String>),
    ) -> ToolTrace {
        let output = &result.output;
        let (preview, output_rules) = crate::trace::redact_preview(output, OUTPUT_PREVIEW_CHARS);
        // A credential the arguments declared secret is withheld from the
        // output too: a tool that echoes what it was given would otherwise put
        // the value back with the key that named it stripped away, which is
        // exactly where a key-based rule cannot reach.
        let (output_preview, withheld) = withhold(&preview, &self.secrets);
        if withheld > 0 {
            self.redactions.push("echoed_secret".to_string());
        }
        self.redactions.extend(output_rules);
        self.redactions.sort();
        self.redactions.dedup();

        // The gate/guard/scope time is whatever is left once the ask and the
        // tool are accounted for — measured rather than inferred, because the
        // three are separated by awaits, not by branches.
        self.timing.decision_ms = started
            .elapsed()
            .as_millis()
            .saturating_sub(u128::from(self.timing.ask_ms) + u128::from(self.timing.exec_ms))
            as u64;
        self.timing.total_ms = started.elapsed().as_millis() as u64;

        let trace = ToolTrace {
            schema: TRACE_SCHEMA,
            trace_id: self.trace_id,
            session_id: ids.0,
            task_id: ids.1,
            turn: self.turn,
            tool_call_id: self.tool_call_id,
            tool: self.tool,
            caller: self.caller,
            effect: self.effect,
            mode: self.mode,
            scope: self.scope,
            arguments: self.arguments,
            decision: self.decision,
            guard: self.guard,
            answer: self.answer,
            result: TraceResult {
                is_error: result.is_error,
                truncated: result.truncated,
                original_bytes: result.original_bytes,
                result_bytes: output.len(),
                output_preview,
                output_digest: crate::trace::stable_digest(output.as_bytes()),
            },
            redactions: self.redactions,
            timing: self.timing,
            started_at_ms: self.started_at_ms,
            finished_at_ms: crate::trace::now_ms(),
        };
        trace
    }
}

/// An id unique to this call: the provider's id when there is one, plus the
/// start instant so two runs of the same conversation cannot collide.
fn trace_id(call_id: &str, started: Instant) -> String {
    format!(
        "{}-{:x}",
        if call_id.is_empty() { "call" } else { call_id },
        started.elapsed().as_nanos()
            ^ crate::trace::now_ms() as u128
            ^ u128::from(std::process::id())
    )
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

/// Concrete `SubToolInvoker` implementation that routes sub-tool calls through `ToolExecutor`.
#[derive(Clone)]
pub struct ExecutorSubToolInvoker {
    executor: ToolExecutor,
    timeout: Option<Duration>,
}

impl ExecutorSubToolInvoker {
    pub fn new(executor: ToolExecutor) -> Self {
        Self {
            executor,
            timeout: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl std::fmt::Debug for ExecutorSubToolInvoker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExecutorSubToolInvoker(..)")
    }
}

#[async_trait]
impl SubToolInvoker for ExecutorSubToolInvoker {
    async fn invoke_sub_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        let call = ToolCall::new_function(
            ctx.tool_call_id.clone(),
            tool_name,
            serde_json::to_string(&arguments).map_err(|e| e.to_string())?,
        );
        let result = self
            .executor
            .run_tool(&call, ctx.clone(), self.timeout)
            .await;
        if result.is_error {
            Ok(CallToolResult::error_text(result.output))
        } else {
            Ok(CallToolResult::success_text(result.output))
        }
    }
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
