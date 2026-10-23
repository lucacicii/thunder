use crate::budget::LoopBudget;
use crate::budget::{BudgetLimit, BudgetState, BudgetUsage};
use crate::cache_warmer::{self, WarmSnapshot};
use crate::context::{repair_tool_call_pairing, ContextBuffer, ContextPruningConfig};
use crate::conversation::now_ms;
use crate::cycles::{CycleAction, CycleDetector};
use crate::gateway::{KvCacheShield, META_CALL_TOOL, META_DISCOVER_TOOLS};
use crate::llm::{LlmDriver, ToolCallRequest};
use crate::loop_guard::LoopGuard;
use crate::middleware::MiddlewarePipeline;
use crate::pause::PauseGate;
use crate::progress::{ProgressTracker, StallConfig};
use crate::pruning::{
    extract_context_overflow_limit, is_context_overflow_error, ContextPruner, PruneResult,
    Summarizer,
};
use crate::steer::SteerQueues;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thunder_executor::ScratchpadManager;
use thunder_protocol::{
    AgentEvent, AgentStats, ChatMessage, EventSink, ExecutionContext, FinishReason, LLMClientTrait,
    ProgressKind, PromptCacheWarmSettings, ThunderEvent, ToolCall, ToolDefinition,
    ToolExecResultPayload, ToolExecutionError, ToolExecutionResult, TurnStats,
};
use thunder_registry::{CatalogSnapshot, ToolRegistry};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Configuration for a ReAct Agent Loop session (§2.2).
#[derive(Clone, Debug)]
pub struct AgentLoopConfig {
    pub session_id: String,
    pub workspace_cwd: PathBuf,
    pub system_prompt: String,
    pub model: String,
    pub pruning: ContextPruningConfig,
    pub tool_timeout: Duration,
    pub cache_warm_settings: Option<PromptCacheWarmSettings>,
    /// Run-level ceilings. Phase C observes them; phase D enforces them.
    pub budget: LoopBudget,
    /// Stall thresholds for the progress tracker.
    pub stall: StallConfig,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            session_id: "sess-default".to_string(),
            workspace_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            system_prompt: "You are Thunder Agent, a microkernel coding assistant.".to_string(),
            model: "gpt-4o".to_string(),
            pruning: ContextPruningConfig::default(),
            tool_timeout: Duration::from_secs(30),
            cache_warm_settings: None,
            budget: LoopBudget::default(),
            stall: StallConfig::default(),
        }
    }
}

/// Summary returned when an Agent Loop finishes a user prompt.
#[derive(Clone, Debug)]
pub struct AgentLoopOutcome {
    pub final_response: String,
    pub turns_executed: usize,
    pub compactions_triggered: usize,
    pub system_faults: Vec<String>,
    pub stats: AgentStats,
    /// The authoritative reason the loop stopped, even when `final_response` came
    /// from a wrap-up turn and therefore exists (see [`AgentLoopOutcome::wire_finish_reason`]).
    pub finish_reason: FinishReason,
}

impl AgentLoopOutcome {
    /// Wire/client-facing finish reason under the single rule from
    /// `docs/agent-loop-budget-and-recovery.md` §3.4: a run that produced an answer
    /// reports `done`, and only a run with no answer at all reports why it stopped.
    /// Clients rely on this — `agent-resume-panel` prints "… without producing an
    /// answer" for any non-`done` reason.
    pub fn wire_finish_reason(&self) -> &'static str {
        if self.final_response.trim().is_empty() {
            match self.finish_reason {
                FinishReason::Done => "done",
                FinishReason::MaxTurnsExceeded => "max_turns_exceeded",
                FinishReason::BudgetExceeded => "budget_exceeded",
                FinishReason::Cancelled => "cancelled",
                FinishReason::Error => "error",
            }
        } else {
            "done"
        }
    }

    /// True when the loop stopped for a reason other than a natural answer; the
    /// content is still usable, so clients should offer to continue rather than
    /// present a failure.
    pub fn stopped_early(&self) -> bool {
        !self.final_response.trim().is_empty() && self.finish_reason != FinishReason::Done
    }
}

/// Microkernel ReAct Agent Loop state machine (§1, §4.1, §4.3, §5.3).
///
/// Pure microkernel design:
/// - Only drives the ReAct inference loop, token window budget, and tool gateway dispatch.
/// - Does not hardcode any concrete business tools, filesystem instruction parsers, or shell AST analyzers.
/// - Provides extensible extension points via `ToolMiddleware`, `EventSink`, `PauseGate`, and `SteerQueues`.
pub struct ReActAgentLoop {
    config: AgentLoopConfig,
    registry: ToolRegistry,
    llm: Arc<dyn LlmDriver>,
    summarizer_client: Option<Arc<dyn LLMClientTrait>>,
    summary_semaphore: Arc<Semaphore>,
    pruner: ContextPruner,
    loop_guard: LoopGuard,
    progress: ProgressTracker,
    cycles: CycleDetector,
    pause_gate: Option<Arc<PauseGate>>,
    steer_queues: Option<Arc<SteerQueues>>,
    scratchpad: Option<ScratchpadManager>,
    cancellation_token: CancellationToken,
    middleware: MiddlewarePipeline,
    context: ContextBuffer,
    kv_shield: KvCacheShield,
    event_sink: Option<Arc<dyn EventSink>>,
}

impl ReActAgentLoop {
    pub fn new(config: AgentLoopConfig, registry: ToolRegistry, llm: Arc<dyn LlmDriver>) -> Self {
        let initial_snapshot = registry.current();
        let kv_shield = KvCacheShield::new(&initial_snapshot);
        let context = ContextBuffer::with_system_prompt(&config.system_prompt);
        let pruner = ContextPruner::new(config.pruning.clone());
        let summarizer_client = llm.as_stream_client();

        Self {
            config,
            registry,
            llm,
            summarizer_client,
            summary_semaphore: Arc::new(Semaphore::new(2)),
            pruner,
            loop_guard: LoopGuard::default(),
            progress: ProgressTracker::default(),
            cycles: CycleDetector::default(),
            pause_gate: None,
            steer_queues: None,
            scratchpad: None,
            cancellation_token: CancellationToken::new(),
            middleware: MiddlewarePipeline::new(),
            context,
            kv_shield,
            event_sink: None,
        }
    }

    pub fn with_summarizer_client(mut self, client: Arc<dyn LLMClientTrait>) -> Self {
        self.summarizer_client = Some(client);
        self
    }

    pub fn with_loop_guard(mut self, loop_guard: LoopGuard) -> Self {
        self.loop_guard = loop_guard;
        self
    }

    pub fn with_pause_gate(mut self, gate: Arc<PauseGate>) -> Self {
        self.pause_gate = Some(gate);
        self
    }

    pub fn with_steer_queues(mut self, queues: Arc<SteerQueues>) -> Self {
        self.steer_queues = Some(queues);
        self
    }

    pub fn with_scratchpad(mut self, scratchpad: ScratchpadManager) -> Self {
        self.scratchpad = Some(scratchpad);
        self
    }

    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    pub fn with_initial_messages(mut self, messages: Vec<ChatMessage>) -> Self {
        if !messages.is_empty() {
            // Heal conversations persisted by older builds: one unpaired `tool_call`
            // makes the whole prefix unreplayable (see `repair_tool_call_pairing`).
            let (messages, repaired) = repair_tool_call_pairing(messages);
            if repaired > 0 {
                tracing::warn!(
                    session_id = %self.config.session_id,
                    repaired,
                    "repaired unpaired tool_calls in restored conversation"
                );
            }
            self.context = ContextBuffer::with_messages(messages);
        }
        self
    }

    pub fn with_middleware(mut self, middleware: MiddlewarePipeline) -> Self {
        self.middleware = middleware;
        self
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    pub fn set_llm(&mut self, llm: Arc<dyn LlmDriver>) {
        self.llm = llm;
    }

    pub fn context(&self) -> &ContextBuffer {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut ContextBuffer {
        &mut self.context
    }

    pub async fn compact_now(&mut self) -> PruneResult {
        let summarizer = self.build_summarizer();
        let cancel = self.cancellation_token.clone();
        self.pruner
            .prune(&mut self.context, summarizer.as_ref(), &cancel)
            .await
    }

    /// Run the ReAct state machine for a user prompt (§1, §2.2).
    pub async fn run(
        &mut self,
        user_prompt: impl Into<String>,
    ) -> Result<AgentLoopOutcome, String> {
        let run_start = Instant::now();
        cache_warmer::invalidate(&self.config.session_id);
        self.loop_guard.reset();
        self.progress = ProgressTracker::from_config(self.config.stall);
        self.cycles.reset();
        let mut budget = BudgetState::new(self.config.budget.clone(), run_start);
        self.context.push(ChatMessage::user(user_prompt));
        let mut system_faults = Vec::new();
        let mut compactions_triggered = 0usize;
        let mut stats = AgentStats::default();
        let cancel = self.cancellation_token.clone();

        // Single source of truth: the loop bound is the budget's turn ceiling.
        // `0` means unlimited, so the loop relies on the other limits (and on the
        // user) to end it.
        let turn_cap = if self.config.budget.max_turns == 0 {
            usize::MAX
        } else {
            self.config.budget.max_turns
        };
        for turn in 1..=turn_cap {
            if cancel.is_cancelled() {
                self.emit_agent(AgentEvent::LoopComplete {
                    finish_reason: FinishReason::Cancelled,
                    final_content: None,
                    stats: stats.clone(),
                });
                return Err("Task cancelled".to_string());
            }

            tokio::task::yield_now().await;

            if let Some(ref gate) = self.pause_gate {
                gate.wait_if_paused().await;
            }

            let turn_start = Instant::now();
            budget.record_turn();

            // Phase C: observe the budget. A near-limit crossing is announced once per
            // run so a client can warn the user; nothing is enforced until phase D.
            match budget.check() {
                BudgetUsage::NearLimit(limit) => {
                    let detail = format!(
                        "{}% of {} used ({} turn(s), {} token(s), {:.0}s elapsed)",
                        (self.config.budget.warn_at * 100.0).round(),
                        limit.as_str(),
                        budget.turns_used(),
                        budget.tokens_used(),
                        budget.elapsed().as_secs_f32(),
                    );
                    self.emit_budget_notice(turn, "near_limit", &detail);
                }
                BudgetUsage::Exhausted(limit) => {
                    // A turn ceiling is reported as such: clients have distinct copy for
                    // "maximum number of turns" and "token budget".
                    let (reason, reason_label) = match limit {
                        BudgetLimit::Turns => {
                            (FinishReason::MaxTurnsExceeded, "max_turns_exceeded")
                        }
                        _ => (FinishReason::BudgetExceeded, "budget_exceeded"),
                    };
                    let detail = format!(
                        "{} limit reached ({} turn(s), {} token(s), {:.0}s elapsed)",
                        limit.as_str(),
                        budget.turns_used(),
                        budget.tokens_used(),
                        budget.elapsed().as_secs_f32(),
                    );
                    return Ok(self
                        .wrap_up_turn(
                            reason,
                            turn,
                            reason_label,
                            &detail,
                            run_start,
                            stats,
                            compactions_triggered,
                            system_faults,
                        )
                        .await);
                }
                BudgetUsage::Ok => {}
            }

            // 1. Lock-free snapshot acquisition at Turn Start (§4.1):
            // Freezes the tool view for the entire turn so mid-turn hot-reloads cannot cause structural drift.
            let turn_snapshot = self.registry.current();
            self.emit(ThunderEvent::TurnStarted {
                turn,
                snapshot_version: turn_snapshot.version,
            });
            self.emit_agent(AgentEvent::TurnStart {
                turn,
                timestamp: now_ms(),
                estimated_prompt_tokens: self.context.estimated_tokens(),
            });

            // 2. Progressive disclosure check (§4.3):
            // If new scripts or MCP servers were mounted since last turn, inject a lightweight
            // `<system_notification>` while keeping top-level `tools` 100% static.
            if let Some(notification) = self.kv_shield.poll_registry_update(&turn_snapshot) {
                self.context.inject_system_notification(notification);
            }

            // 3. Pi-style Checkpoint Compaction check before calling the LLM:
            // Keeps message prefix 100% byte-identical until `estimated > window - reserve`.
            if self.pruner.should_compact(self.context.estimated_tokens()) {
                let summarizer = self.build_summarizer();
                let prune_res = self
                    .pruner
                    .prune(&mut self.context, summarizer.as_ref(), &cancel)
                    .await;
                if prune_res.pruned {
                    compactions_triggered += 1;
                    self.emit_agent(AgentEvent::ContextCompacted {
                        turn: Some(turn),
                        tokens_before: prune_res.tokens_before,
                        tokens_after: prune_res.tokens_after,
                    });
                }
            }

            // 4. Invoke LLM with frozen top-level tools (`KvCacheShield::top_level_tools`),
            // with automatic context-overflow detection and self-healing retry.
            let turn_resp = match self
                .llm
                .complete_streaming(
                    &self.context.get_messages(),
                    self.kv_shield.top_level_tools(),
                    turn,
                    self.event_sink.as_ref(),
                    &cancel,
                )
                .await
            {
                Ok(resp) => resp,
                Err(err) if is_context_overflow_error(&err) => {
                    if let Some(detected_limit) = extract_context_overflow_limit(&err) {
                        self.pruner
                            .update_max_tokens(detected_limit.saturating_sub(4096));
                    } else {
                        let halved = (self.pruner.max_tokens() * 3 / 4).max(2048);
                        self.pruner.update_max_tokens(halved);
                    }
                    let summarizer = self.build_summarizer();
                    let prune_res = self
                        .pruner
                        .prune(&mut self.context, summarizer.as_ref(), &cancel)
                        .await;
                    if !prune_res.pruned {
                        self.pruner.emergency_trim(&mut self.context);
                    }
                    compactions_triggered += 1;
                    self.llm
                        .complete_streaming(
                            &self.context.get_messages(),
                            self.kv_shield.top_level_tools(),
                            turn,
                            self.event_sink.as_ref(),
                            &cancel,
                        )
                        .await?
                }
                Err(err) => {
                    self.emit_agent(AgentEvent::Error {
                        turn: Some(turn),
                        message: err.clone(),
                        recoverable: false,
                    });
                    return Err(err);
                }
            };

            budget.record_tokens(turn_resp.prompt_tokens, turn_resp.completion_tokens);
            stats.total_turns = turn;
            stats.total_prompt_tokens += turn_resp.prompt_tokens;
            stats.total_completion_tokens += turn_resp.completion_tokens;
            stats.total_cached_tokens += turn_resp.cached_tokens;

            if let (Some(ref warm_cfg), Some(ref client)) =
                (&self.config.cache_warm_settings, &self.summarizer_client)
            {
                if turn_resp.prompt_tokens > 0 {
                    let warm_snap = WarmSnapshot {
                        model: self.config.model.clone(),
                        messages: self.context.get_messages(),
                        tools: self
                            .kv_shield
                            .top_level_tools()
                            .iter()
                            .map(ToolDefinition::from)
                            .collect(),
                        temperature: None,
                        top_p: None,
                        thinking_level: None,
                        session_id: self.config.session_id.clone(),
                        prompt_tokens: turn_resp.prompt_tokens,
                    };
                    cache_warmer::schedule(Arc::clone(client), warm_snap, warm_cfg.clone());
                }
            }

            let wire_calls: Option<Vec<ToolCall>> = if turn_resp.tool_calls.is_empty() {
                None
            } else {
                let calls: Vec<ToolCall> = turn_resp
                    .tool_calls
                    .iter()
                    .map(ToolCallRequest::to_wire_tool_call)
                    .collect();
                for wc in &calls {
                    self.emit_agent(AgentEvent::ToolCallReady {
                        turn,
                        tool_call: wc.clone(),
                    });
                }
                Some(calls)
            };

            let content_opt = if turn_resp.content.is_empty() && wire_calls.is_some() {
                None
            } else {
                Some(turn_resp.content.clone())
            };

            let turn_stats = TurnStats {
                turn,
                prompt_tokens: turn_resp.prompt_tokens,
                completion_tokens: turn_resp.completion_tokens,
                cached_tokens: turn_resp.cached_tokens,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
                total_tokens: turn_resp.prompt_tokens + turn_resp.completion_tokens,
                tool_calls_count: turn_resp.tool_calls.len(),
                duration_ms: turn_start.elapsed().as_millis() as u64,
            };

            self.context.push(ChatMessage::assistant_turn(
                content_opt,
                wire_calls,
                turn_resp.reasoning.clone(),
                Some(turn_stats.clone()),
            ));

            if turn_resp.tool_calls.is_empty() {
                self.emit_agent(AgentEvent::TurnEnd {
                    turn,
                    finish_reason: "stop".to_string(),
                    stats: turn_stats,
                });

                if let Some(ref queues) = self.steer_queues {
                    let mut injected = queues.steering.drain();
                    let mut behavior = "steer";
                    if injected.is_empty() {
                        injected = queues.follow_up.drain();
                        behavior = "follow_up";
                    }
                    if !injected.is_empty() {
                        for msg in injected {
                            if let Some(text) = msg.content_str() {
                                self.emit_agent(AgentEvent::SteerAccepted {
                                    turn,
                                    behavior: behavior.to_string(),
                                    message: text.to_string(),
                                    image_count: msg.image_count(),
                                });
                            }
                            self.context.push(msg);
                        }
                        continue;
                    }
                }

                stats.total_duration_ms = run_start.elapsed().as_millis() as u64;
                stats.mutations = self.progress.total_mutations();
                stats.novel_observations = self.progress.total_novel_observations();
                cache_warmer::mark_idle(&self.config.session_id);
                self.emit(ThunderEvent::AgentFinished {
                    turns: turn,
                    final_response: turn_resp.content.clone(),
                });
                self.emit_agent(AgentEvent::LoopComplete {
                    finish_reason: FinishReason::Done,
                    final_content: Some(turn_resp.content.clone()),
                    stats: stats.clone(),
                });
                stats.finish_reason = Some(FinishReason::Done);
                return Ok(AgentLoopOutcome {
                    final_response: turn_resp.content,
                    turns_executed: turn,
                    compactions_triggered,
                    system_faults,
                    stats,
                    finish_reason: FinishReason::Done,
                });
            }

            // 5. Execute requested tool calls against the frozen `turn_snapshot` (§1).
            for (call_idx, call) in turn_resp.tool_calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    self.close_pending_tool_calls(&turn_resp.tool_calls, call_idx, "run cancelled");
                    return Err("Task cancelled".to_string());
                }
                if let Some(ref gate) = self.pause_gate {
                    gate.wait_if_paused().await;
                }

                let args_serialized =
                    serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string());
                let rep_warning = self
                    .loop_guard
                    .record_and_check_repetition(&call.name, &args_serialized);

                // Cycle detection works on the *action* signature (tool + arguments), so
                // it is decidable before dispatch and can stop the call from running.
                let action = crate::cycles::fingerprint_action(
                    turn_snapshot
                        .descriptors
                        .get(&call.name)
                        .map(|d| d.progress)
                        .unwrap_or(ProgressKind::Neutral),
                    &call.name,
                    &args_serialized,
                );
                let cycle_warning = match self.cycles.record(action) {
                    CycleAction::SoftWarn(hit) => Some(format!(
                        "[System Notice: you have repeated the same {}-step cycle {} times \
                         without new information. Change your approach: verify the current \
                         result, pick a different source of information, or summarize and stop.]",
                        hit.period, hit.cycles
                    )),
                    CycleAction::HardAbort(hit) => {
                        self.close_pending_tool_calls(
                            &turn_resp.tool_calls,
                            call_idx,
                            "loop cycle detected",
                        );
                        let detail = format!(
                            "the same {}-step cycle repeated {} times",
                            hit.period, hit.cycles
                        );
                        return Ok(self
                            .wrap_up_turn(
                                FinishReason::BudgetExceeded,
                                turn,
                                "stalled",
                                &detail,
                                run_start,
                                stats,
                                compactions_triggered,
                                system_faults,
                            )
                            .await);
                    }
                    CycleAction::None => None,
                };

                if self.loop_guard.is_repetition_limit_exceeded() {
                    self.close_pending_tool_calls(
                        &turn_resp.tool_calls,
                        call_idx,
                        "loop guard: identical tool call repeated without progress",
                    );
                    cache_warmer::mark_idle(&self.config.session_id);
                    return Err(format!(
                        "LoopGuard circuit breaker: tool '{}' repeated identically too many times",
                        call.name
                    ));
                }

                self.emit_agent(AgentEvent::ToolExecStart {
                    turn,
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                });

                let start = Instant::now();
                let mut normalized = self.dispatch_single_call(call, &turn_snapshot).await;
                let duration_ms = start.elapsed().as_millis() as u64;

                stats.total_tool_executions += 1;
                stats.total_tool_time_ms += duration_ms;

                self.loop_guard.record_tool_result(!normalized.is_success());

                let mut truncated = false;
                let mut original_bytes = 0usize;

                if let (Some(ref sp), ToolExecutionResult::Success { ref mut output }) =
                    (&self.scratchpad, &mut normalized)
                {
                    original_bytes = output.len();
                    if original_bytes > sp.threshold_bytes() {
                        if let Ok(handle_text) = sp
                            .process_tool_output(&call.name, turn, output.clone())
                            .await
                        {
                            *output = handle_text;
                            truncated = true;
                        }
                    }
                }

                if let ToolExecutionResult::SystemFault { ref reason } = normalized {
                    system_faults.push(reason.clone());
                    self.emit(ThunderEvent::SystemFaultDetected {
                        tool_name: call.name.clone(),
                        reason: reason.clone(),
                    });
                }

                self.emit(ThunderEvent::ToolCallCompleted {
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    result: normalized.clone(),
                    duration_ms,
                });

                let mut result_text = normalized.to_llm_message();
                if original_bytes == 0 {
                    original_bytes = result_text.len();
                }

                // Phase B: progress observation only. The digest never changes control
                // flow yet; it feeds tracing + AgentStats so the stall thresholds can be
                // calibrated against real runs before they are enforced.
                let progress_kind = turn_snapshot
                    .descriptors
                    .get(&call.name)
                    .map(|d| d.progress)
                    .unwrap_or(ProgressKind::Neutral);
                let novel =
                    self.progress
                        .record(progress_kind, &call.name, &args_serialized, &result_text);
                tracing::debug!(
                    turn,
                    tool = %call.name,
                    kind = ?progress_kind,
                    novel,
                    mutations = self.progress.total_mutations(),
                    novel_observations = self.progress.total_novel_observations(),
                    mutation_free_streak = self.progress.mutation_free_streak(),
                    no_novel_streak = self.progress.no_novel_output_streak(),
                    "tool progress signal"
                );

                for warn in [rep_warning, cycle_warning].into_iter().flatten() {
                    result_text.push_str("\n\n");
                    result_text.push_str(&warn);
                }

                self.emit_agent(AgentEvent::ToolExecResult {
                    turn,
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    result: ToolExecResultPayload {
                        tool_call_id: call.id.clone(),
                        output: result_text.clone(),
                        is_error: !normalized.is_success(),
                        truncated,
                        original_bytes,
                        duration_ms,
                    },
                });

                self.context.push(ChatMessage::tool(
                    call.id.clone(),
                    result_text,
                    Some(call.name.clone()),
                ));

                if self.loop_guard.is_circuit_broken() {
                    self.close_pending_tool_calls(
                        &turn_resp.tool_calls,
                        call_idx + 1,
                        "loop guard: too many consecutive tool errors",
                    );
                    cache_warmer::mark_idle(&self.config.session_id);
                    return Err(format!(
                        "LoopGuard circuit breaker: {} consecutive tool execution errors",
                        self.loop_guard.consecutive_errors()
                    ));
                }
            }

            self.emit_agent(AgentEvent::TurnEnd {
                turn,
                finish_reason: "tool_calls".to_string(),
                stats: turn_stats,
            });

            // Stall detection: the run already started changing things and then stopped
            // producing anything new. Wrap up rather than burning the remaining budget.
            if let Some(signal) = self.progress.on_turn_end() {
                stats.stall_events += 1;
                let detail = match signal {
                    crate::progress::StallSignal::MutationFree(turns) => {
                        format!("{turns} turn(s) without a new state mutation")
                    }
                    crate::progress::StallSignal::NoNovelObservation(turns) => {
                        format!("{turns} turn(s) without any new tool result")
                    }
                };
                tracing::warn!(
                    turn,
                    %detail,
                    mutations = self.progress.total_mutations(),
                    novel_observations = self.progress.total_novel_observations(),
                    "stall detected; wrapping up"
                );
                return Ok(self
                    .wrap_up_turn(
                        FinishReason::BudgetExceeded,
                        turn,
                        "stalled",
                        &detail,
                        run_start,
                        stats,
                        compactions_triggered,
                        system_faults,
                    )
                    .await);
            }

            // Drain any mid-turn steering messages before the next turn starts.
            if let Some(ref queues) = self.steer_queues {
                for msg in queues.steering.drain() {
                    if let Some(text) = msg.content_str() {
                        self.emit_agent(AgentEvent::SteerAccepted {
                            turn,
                            behavior: "steer".to_string(),
                            message: text.to_string(),
                            image_count: msg.image_count(),
                        });
                    }
                    self.context.push(msg);
                }
            }
        }

        tracing::debug!(
            turns = stats.total_turns,
            mutations = self.progress.total_mutations(),
            novel_observations = self.progress.total_novel_observations(),
            stall_events = stats.stall_events,
            "agent loop exhausted its turn budget"
        );
        let detail = format!("{turn_cap} turn(s) used");
        Ok(self
            .wrap_up_turn(
                FinishReason::MaxTurnsExceeded,
                turn_cap,
                "max_turns_exceeded",
                &detail,
                run_start,
                stats,
                compactions_triggered,
                system_faults,
            )
            .await)
    }

    /// Close a run that hit a ceiling by giving the model one final, tool-free turn.
    ///
    /// Phase D never reports these outcomes as errors: the user gets a summary of what
    /// was done and what remains. If the wrap-up call itself fails, the last assistant
    /// text in the context is reused as the answer; only when there is truly nothing to
    /// show does the outcome keep a non-`done` reason on the wire.
    async fn wrap_up_turn(
        &mut self,
        reason: FinishReason,
        turn: usize,
        reason_label: &str,
        detail: &str,
        run_start: Instant,
        mut stats: AgentStats,
        compactions_triggered: usize,
        system_faults: Vec<String>,
    ) -> AgentLoopOutcome {
        let cancel = self.cancellation_token.clone();
        // Single place where a ceiling announces itself, so clients get exactly one
        // machine signal per stop regardless of which limit fired.
        self.emit_budget_notice(turn, reason_label, detail);
        self.context.inject_system_notification(format!(
            "<budget_notice>\nThe run is stopping: {reason_label} ({detail}).\n\
             Do not call any more tools. Summarize in at most 150 words:\n\
             1) what is done, 2) what is unfinished, 3) the next concrete step.\n</budget_notice>"
        ));

        let wrapped = self
            .llm
            .complete_streaming(
                &self.context.get_messages(),
                &[],
                turn,
                self.event_sink.as_ref(),
                &cancel,
            )
            .await;

        let content = match wrapped {
            Ok(resp) => {
                stats.total_turns = turn;
                stats.total_prompt_tokens += resp.prompt_tokens;
                stats.total_completion_tokens += resp.completion_tokens;
                self.context
                    .push(ChatMessage::assistant_text(resp.content.clone()));
                let turn_stats = TurnStats {
                    turn,
                    prompt_tokens: resp.prompt_tokens,
                    completion_tokens: resp.completion_tokens,
                    cached_tokens: resp.cached_tokens,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                    total_tokens: resp.prompt_tokens + resp.completion_tokens,
                    tool_calls_count: 0,
                    duration_ms: 0,
                };
                self.emit_agent(AgentEvent::TurnEnd {
                    turn,
                    finish_reason: reason_label.to_string(),
                    stats: turn_stats,
                });
                resp.content
            }
            Err(err) => {
                tracing::warn!(
                    turn,
                    error = %err,
                    "wrap-up turn failed; falling back to the last assistant text"
                );
                self.last_assistant_text().unwrap_or_default()
            }
        };

        // No fabricated placeholder: an empty answer must stay empty so the wire rule
        // in §3.4 can surface the real stop reason to the client.
        let final_content = if content.trim().is_empty() {
            String::new()
        } else {
            content
        };

        stats.total_duration_ms = run_start.elapsed().as_millis() as u64;
        stats.mutations = self.progress.total_mutations();
        stats.novel_observations = self.progress.total_novel_observations();
        stats.finish_reason = Some(reason);
        cache_warmer::mark_idle(&self.config.session_id);
        self.emit(ThunderEvent::AgentFinished {
            turns: turn,
            final_response: final_content.clone(),
        });
        self.emit_agent(AgentEvent::LoopComplete {
            finish_reason: reason,
            final_content: (!final_content.is_empty()).then(|| final_content.clone()),
            stats: stats.clone(),
        });

        AgentLoopOutcome {
            final_response: final_content,
            turns_executed: turn,
            compactions_triggered,
            system_faults,
            stats,
            finish_reason: reason,
        }
    }

    /// Last assistant text still present in the context, used as a fallback answer
    /// when the wrap-up call cannot run.
    fn last_assistant_text(&self) -> Option<String> {
        self.context
            .get_messages()
            .iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::Assistant { content, .. } => content
                    .as_deref()
                    .filter(|c| !c.trim().is_empty())
                    .map(str::to_string),
                _ => None,
            })
    }

    /// Fill in placeholder tool results for every call in `calls[start..]`.
    ///
    /// An early exit must never leave an assistant message whose `tool_calls` have no
    /// matching tool messages: providers reject that shape with HTTP 400, and the
    /// daemon persists the conversation context verbatim, so a single unpaired
    /// `tool_call` poisons every later run of that session.
    fn close_pending_tool_calls(&mut self, calls: &[ToolCallRequest], start: usize, reason: &str) {
        if start >= calls.len() {
            return;
        }
        for call in calls.iter().skip(start) {
            let placeholder = format!(
                "[aborted: {reason}. This tool call was not executed because the run ended early.]"
            );
            self.context.push(ChatMessage::tool(
                call.id.clone(),
                placeholder,
                Some(call.name.clone()),
            ));
        }
    }

    fn build_summarizer(&self) -> Option<Summarizer> {
        let client = self.summarizer_client.as_ref()?;
        Some(Summarizer::new(
            Arc::clone(client),
            &self.config.model,
            &self.config.pruning,
            Arc::clone(&self.summary_semaphore),
        ))
    }

    async fn dispatch_single_call(
        &self,
        call: &ToolCallRequest,
        snapshot: &CatalogSnapshot,
    ) -> ToolExecutionResult {
        let exec_ctx = ExecutionContext::new(
            &self.config.session_id,
            &self.config.workspace_cwd,
            self.config.tool_timeout,
        );

        // Meta-tool 1: `thunder__discover_tools` (§4.3)
        if call.name == META_DISCOVER_TOOLS {
            self.emit(ThunderEvent::ToolCallDispatched {
                call_id: call.id.clone(),
                tool_name: call.name.clone(),
                via_gateway: true,
            });
            let output = KvCacheShield::execute_discover_tools(snapshot, &call.arguments);
            return ToolExecutionResult::Success { output };
        }

        // Meta-tool 2: `thunder__call_tool` (§4.3)
        if call.name == META_CALL_TOOL {
            let Some(target_name) = call.arguments.get("name").and_then(|v| v.as_str()) else {
                return ToolExecutionResult::normalize(Err(
                    ToolExecutionError::SerializationError(
                        "thunder__call_tool requires string parameter 'name'".to_string(),
                    ),
                ));
            };
            let target_args: Value = call
                .arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));

            self.emit(ThunderEvent::ToolCallDispatched {
                call_id: call.id.clone(),
                tool_name: target_name.to_string(),
                via_gateway: true,
            });

            // Pass through the generic host-configured MiddlewarePipeline
            if let Err(mw_err) = self
                .middleware
                .check(target_name, &target_args, &exec_ctx)
                .await
            {
                return ToolExecutionResult::normalize(Err(mw_err));
            }

            let raw_res =
                ToolRegistry::call_with_snapshot(snapshot, target_name, target_args, &exec_ctx)
                    .await;
            return ToolExecutionResult::normalize(raw_res);
        }

        // Direct top-level tool call
        self.emit(ThunderEvent::ToolCallDispatched {
            call_id: call.id.clone(),
            tool_name: call.name.clone(),
            via_gateway: false,
        });

        if let Err(mw_err) = self
            .middleware
            .check(&call.name, &call.arguments, &exec_ctx)
            .await
        {
            return ToolExecutionResult::normalize(Err(mw_err));
        }

        let raw_res = ToolRegistry::call_with_snapshot(
            snapshot,
            &call.name,
            call.arguments.clone(),
            &exec_ctx,
        )
        .await;
        ToolExecutionResult::normalize(raw_res)
    }

    fn emit(&self, event: ThunderEvent) {
        if let Some(ref sink) = self.event_sink {
            sink.on_event(event);
        }
    }

    /// Phase C transport for budget/recovery signals: `Custom` keeps the wire shape
    /// open-ended so clients that do not know the `kind` simply ignore it.
    fn emit_budget_notice(&self, turn: usize, reason: &str, detail: &str) {
        self.emit_agent(AgentEvent::Custom {
            kind: "budget_notice".to_string(),
            payload: json!({
                "turn": turn,
                "reason": reason,
                "detail": detail,
            }),
        });
    }

    fn emit_agent(&self, event: AgentEvent) {
        if let Some(ref sink) = self.event_sink {
            sink.on_agent_event(event);
        }
    }
}
