use crate::core::context::ContextBuffer;
use crate::core::state::{AgentStateTracker, LoopStatus};
use crate::core::steer::QueueBehavior;
use crate::loop_engine::guard::LoopGuard;
use crate::loop_engine::handle::{AgentHandle, RunningGuard};
use crate::loop_engine::hooks::AgentEventDispatcher;
use crate::pruning::strategy::ContextPruner;
use crate::stream::client::{
    ChatRequestOptions, LLMClientTrait, LLMStreamChunk, UnconfiguredLLMClient,
};
use crate::tools::executor::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::tools::scratchpad::ScratchpadManager;
use crate::types::config::AgentConfig;
use crate::types::error::AgentError;
use crate::types::event::{AgentEvent, AgentStats, FinishReason, ObservedEvent, TurnStats};
use crate::types::message::{ChatMessage, Role};
use crate::types::tool::{AgentTool, ToolExecutionResult};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub agent_id: String,
    pub final_content: Option<String>,
    /// The projected context the LLM actually saw (includes any checkpoint
    /// summary message swapped in by compaction). This is what hosts should
    /// feed back on the next run.
    pub messages: Vec<ChatMessage>,
    /// Raw pre-compaction transcript, present only when checkpoint compaction
    /// fired during this run. Pi-style: the original history is never silently
    /// destroyed — hosts may persist it for audit/export.
    pub raw_messages: Option<Vec<ChatMessage>>,
    pub stats: AgentStats,
    pub finish_reason: FinishReason,
}

/// A complete single-agent unit.
///
/// One instance runs at most one task at a time. Parallel work requires
/// constructing another `AgentLoop`. A scheduler (app B) creates many units
/// and calls [`Self::start`] / [`AgentHandle::join`]; it does not drive turns.
pub struct AgentLoop {
    id: String,
    config: AgentConfig,
    llm_client: Arc<dyn LLMClientTrait>,
    tool_registry: ToolRegistry,
    tool_executor: ToolExecutor,
    event_dispatcher: AgentEventDispatcher,
    scratchpad: ScratchpadManager,
    running: Arc<AtomicBool>,
    status: Arc<AtomicU8>,
    /// Cooperative pause gate; shared with any handle that wants to pause this unit.
    pause_gate: Arc<crate::core::pause::PauseGate>,
    /// Steering and follow-up queues; shared with any handle that wants to
    /// inject user input into a run already in flight.
    steer_queues: Arc<crate::core::steer::SteerQueues>,
    /// The judge's inputs, threaded into every pipeline rebuild.
    ///
    /// `None` means "no policy adopted": the guard then enforces only the static
    /// tier. `ui` of `None` inside a policy means every dialog is declined, so a
    /// headless host refuses rather than assuming consent.
    policy: Option<Arc<crate::types::policy::SessionPolicy>>,
    host_ui: Option<Arc<dyn crate::types::ui::HostUi>>,
}

impl AgentLoop {
    pub fn new(config: AgentConfig) -> Self {
        let id = generate_agent_id();
        let scratchpad = ScratchpadManager::new(&id, config.scratchpad.clone());
        let llm_client = Arc::new(UnconfiguredLLMClient);
        let tool_registry = ToolRegistry::new(
            config.max_tool_output_bytes,
            std::time::Duration::from_millis(config.request_timeout_ms),
        )
        .with_scratchpad(scratchpad.clone());

        let ws = config.workspace_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        let tool_executor = ToolExecutor::with_configured_pipeline(
            tool_registry.clone(),
            ws,
            config.extra_workspace_roots.clone(),
            Some(scratchpad.clone()),
            &config.middleware,
            config.permission,
            None,
            None,
        );

        Self {
            id,
            config,
            llm_client,
            tool_registry,
            tool_executor,
            event_dispatcher: AgentEventDispatcher::default(),
            scratchpad,
            running: Arc::new(AtomicBool::new(false)),
            status: Arc::new(AtomicU8::new(LoopStatus::Idle.as_u8())),
            pause_gate: crate::core::pause::PauseGate::new_shared(),
            steer_queues: crate::core::steer::SteerQueues::new_shared(),
            policy: None,
            host_ui: None,
        }
    }

    /// Install the permission policy and the panel it may prompt through.
    ///
    /// Both are held by the unit and travel with every pipeline rebuild, so a
    /// tool registered later is still judged by the same policy. The policy
    /// carries the live mode and remembered rules, so flipping the mode
    /// mid-session takes effect on the next call without rebuilding anything.
    pub fn with_policy(
        mut self,
        policy: Arc<crate::types::policy::SessionPolicy>,
        ui: Arc<dyn crate::types::ui::HostUi>,
    ) -> Self {
        self.policy = Some(policy);
        self.host_ui = Some(ui);
        self.tool_executor = self.rebuild_executor();
        self
    }

    /// Reassemble the pipeline from the current config and registrations.
    fn rebuild_executor(&self) -> ToolExecutor {
        let ws = self.config.workspace_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        ToolExecutor::with_configured_pipeline(
            self.tool_registry.clone(),
            ws,
            self.config.extra_workspace_roots.clone(),
            Some(self.scratchpad.clone()),
            &self.config.middleware,
            self.config.permission,
            self.policy.clone(),
            self.host_ui.clone(),
        )
    }

    /// Assign a stable unit id (used in events, scratchpad isolation, errors).
    /// Recreates the scratchpad so files land under `base_dir/<id>/`.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self.scratchpad = ScratchpadManager::new(&self.id, self.config.scratchpad.clone());
        self.tool_registry.set_scratchpad(self.scratchpad.clone());
        let ws = self.config.workspace_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        self.tool_executor = ToolExecutor::with_configured_pipeline(
            self.tool_registry.clone(),
            ws,
            self.config.extra_workspace_roots.clone(),
            Some(self.scratchpad.clone()),
            &self.config.middleware,
            self.config.permission,
            self.policy.clone(),
            self.host_ui.clone(),
        );
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Read-only access to the tool executor (pipeline + registry).
    pub fn tool_executor(&self) -> &ToolExecutor {
        &self.tool_executor
    }

    pub fn status(&self) -> LoopStatus {
        LoopStatus::from_u8(self.status.load(Ordering::Acquire))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Adopt an externally owned pause gate, so a host can hold and release this
    /// unit without reaching into its internals.
    pub fn with_pause_gate(mut self, gate: Arc<crate::core::pause::PauseGate>) -> Self {
        self.pause_gate = gate;
        self
    }

    /// Adopt externally owned steering / follow-up queues.
    ///
    /// A host that holds the `Arc` can queue user input into a run already in
    /// flight; the loop drains it at turn boundaries. Passing the queues in
    /// (rather than reading them off the handle) is what lets the host keep
    /// rendering its pending list while the run holds the handle.
    pub fn with_steer_queues(mut self, queues: Arc<crate::core::steer::SteerQueues>) -> Self {
        self.steer_queues = queues;
        self
    }

    /// This unit's steering / follow-up queues.
    pub fn steer_queues(&self) -> &Arc<crate::core::steer::SteerQueues> {
        &self.steer_queues
    }

    pub fn with_custom_client(mut self, client: Arc<dyn LLMClientTrait>) -> Self {
        self.llm_client = client;
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn AgentTool>) -> &mut Self {
        self.tool_registry.register(tool);
        let ws = self.config.workspace_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        self.tool_executor = ToolExecutor::with_configured_pipeline(
            self.tool_registry.clone(),
            ws,
            self.config.extra_workspace_roots.clone(),
            Some(self.scratchpad.clone()),
            &self.config.middleware,
            self.config.permission,
            self.policy.clone(),
            self.host_ui.clone(),
        );
        self
    }

    /// Disables atomic transactions on this AgentLoop (ideal for benchmarking or testing raw tool behavior).
    pub fn without_transactions(mut self) -> Self {
        self.config.middleware.enable_transaction = false;
        let ws = self.config.workspace_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        self.tool_executor = ToolExecutor::with_configured_pipeline(
            self.tool_registry.clone(),
            ws,
            self.config.extra_workspace_roots.clone(),
            Some(self.scratchpad.clone()),
            &self.config.middleware,
            self.config.permission,
            self.policy.clone(),
            self.host_ui.clone(),
        );
        self
    }

    /// Injects a completely custom `ToolExecutor` into this AgentLoop.
    pub fn with_tool_executor(mut self, executor: ToolExecutor) -> Self {
        self.tool_executor = executor;
        self
    }

    /// Convenience builder to register standard built-in tools
    /// (bash, read_file, write_file, grep, find, ls).
    pub fn with_builtins(mut self) -> Self {
        self.register_tool(Arc::new(crate::tools::builtin::BashTool::default()));
        self.register_tool(Arc::new(crate::tools::builtin::ReadFileTool::default()));
        self.register_tool(Arc::new(crate::tools::builtin::WriteFileTool::default()));
        self.register_tool(Arc::new(crate::tools::builtin::GrepTool::default()));
        self.register_tool(Arc::new(crate::tools::builtin::FindTool::default()));
        self.register_tool(Arc::new(crate::tools::builtin::ListDirTool::default()));
        self
    }

    pub fn scratchpad(&self) -> &ScratchpadManager {
        &self.scratchpad
    }

    /// Lossy broadcast sidecar. Prefer [`AgentHandle::events`] for a scheduler.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<ObservedEvent> {
        self.event_dispatcher.subscribe()
    }

    /// Non-blocking start. Returns a handle the host (CLI or scheduler B) can
    /// join, cancel, and observe. Fails if this unit is already running.
    pub fn start(
        &self,
        input: impl Into<ContextInput>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<AgentHandle, AgentError> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(AgentError::AlreadyRunning {
                agent_id: self.id.clone(),
            });
        }

        let token = cancel_token.unwrap_or_default();
        let (event_tx, event_rx) = mpsc::channel::<ObservedEvent>(512);
        let (result_tx, result_rx) = oneshot::channel();

        let pause_gate = Arc::clone(&self.pause_gate);
        let steer_queues = Arc::clone(&self.steer_queues);
        self.spawn_loop(input.into(), event_tx, result_tx, token.clone());

        Ok(AgentHandle::new(
            self.id.clone(),
            self.status.clone(),
            self.running.clone(),
            token,
            pause_gate,
            steer_queues,
            result_rx,
            event_rx,
        ))
    }

    /// Closed-loop convenience: start, drain the reliable event stream, join.
    /// Subscribe via [`Self::subscribe_events`] before calling if you need events.
    pub async fn run(
        &self,
        input: impl Into<ContextInput>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<AgentRunResult, AgentError> {
        let mut handle = self.start(input, cancel_token)?;
        if let Some(mut ev) = handle.take_events() {
            tokio::spawn(async move { while ev.recv().await.is_some() {} });
        }
        handle.join().await
    }

    fn spawn_loop(
        &self,
        input: ContextInput,
        event_sender: mpsc::Sender<ObservedEvent>,
        result_tx: oneshot::Sender<AgentRunResult>,
        cancel_token: CancellationToken,
    ) {
        let mut context = match input {
            ContextInput::Text(prompt) => {
                let mut ctx = ContextBuffer::new();
                if let Some(sys) = &self.config.system_prompt {
                    ctx.set_system_prompt(sys);
                }
                ctx.push(ChatMessage::user(prompt));
                ctx
            }
            ContextInput::Messages(messages) => {
                let mut ctx = ContextBuffer::with_messages(messages);
                if let Some(sys) = &self.config.system_prompt {
                    if ctx.is_empty()
                        || ctx
                            .get_entry(0)
                            .map(|e| e.message.role() != Role::System)
                            .unwrap_or(true)
                    {
                        ctx.set_system_prompt(sys);
                    }
                }
                ctx
            }
            ContextInput::Buffer(ctx) => ctx,
        };

        let config = self.config.clone();
        let llm_client = self.llm_client.clone();
        let tool_registry = self.tool_registry.clone();
        let tool_executor = self.tool_executor.clone();
        let dispatcher = self.event_dispatcher.clone();
        let mut pruner = ContextPruner::new(config.pruning.clone());
        let scratchpad = self.scratchpad.clone();
        let agent_id = self.id.clone();
        let status = self.status.clone();
        let running = self.running.clone();
        let pause_gate = Arc::clone(&self.pause_gate);
        let steer_queues = Arc::clone(&self.steer_queues);
        let summarizer = crate::pruning::checkpoint::Summarizer::new(llm_client.clone(), &config);

        tokio::spawn(async move {
            let _busy = RunningGuard::new(running);
            let emitter = Emitter {
                agent_id: agent_id.clone(),
                dispatcher,
                event_sender,
            };

            let mut tracker = AgentStateTracker::new();
            tracker.set_status(LoopStatus::Running);
            status.store(LoopStatus::Running.as_u8(), Ordering::Release);

            // A new run moves the conversation's prefix forward: cancel any
            // warmer still refreshing the stale pre-run prefix.
            if let Some(session_id) = config.session_id.as_deref() {
                crate::cache::warmer::invalidate(session_id);
            }

            let guard_cfg = &config.loop_guard;
            let mut guard = LoopGuard::new(
                guard_cfg.max_history,
                guard_cfg.repetition_threshold,
                guard_cfg.hard_repetition_limit,
                guard_cfg.max_consecutive_errors,
            );
            let mut final_content = None;
            let loop_finish_reason: FinishReason;
            // Raw (pre-compaction) transcript snapshot: activated the first time
            // a checkpoint compaction rewrites the projection, then kept in sync
            // with durable pushes so hosts can persist the original history.
            let mut raw_log: Option<Vec<ChatMessage>> = None;

            // Input queued before the run started (the user may have typed while
            // the previous run was finishing) plus anything queued since. Each
            // entry carries which queue it came from, because that decides when
            // it is allowed to enter.
            let mut pending_messages: Vec<(ChatMessage, QueueBehavior)> = steer_queues
                .steering
                .drain()
                .into_iter()
                .map(|m| (m, QueueBehavior::Steer))
                .collect();

            // Set when a run is cancelled mid-stream. The partial answer lives in
            // the turn's locals, which die with the turn, so it is lifted out here
            // to survive into the transcript.
            let mut cancelled_partial: Option<String> = None;

            loop {
                if let Some(max) = config.max_turns {
                    if tracker.current_turn() >= max {
                        warn!(agent_id = %agent_id, max_turns = max, "Reached configured max_turns threshold");
                        loop_finish_reason = FinishReason::MaxTurnsExceeded;
                        break;
                    }
                }

                if cancel_token.is_cancelled() {
                    info!(agent_id = %agent_id, "Agent loop execution cancelled by token");
                    loop_finish_reason = FinishReason::Cancelled;
                    break;
                }

                if guard.is_circuit_broken() {
                    let err_msg = format!(
                        "Circuit breaker triggered: {} consecutive tool execution failures. Aborting loop to protect budget.",
                        guard.consecutive_errors()
                    );
                    error!(agent_id = %agent_id, %err_msg);
                    emitter
                        .emit(AgentEvent::Error {
                            turn: Some(tracker.current_turn()),
                            message: err_msg,
                            recoverable: false,
                        })
                        .await;
                    loop_finish_reason = FinishReason::Error;
                    break;
                }

                if guard.is_repetition_limit_exceeded() {
                    let err_msg = "Hard repetition limit exceeded: identical tool call repeated excessively. Terminating loop to prevent infinite spin.".to_string();
                    warn!(agent_id = %agent_id, %err_msg);
                    emitter
                        .emit(AgentEvent::Error {
                            turn: Some(tracker.current_turn()),
                            message: err_msg,
                            recoverable: false,
                        })
                        .await;
                    loop_finish_reason = FinishReason::Error;
                    break;
                }

                // Snapshot the raw transcript right before the first checkpoint
                // compaction rewrites history (pi-style: raw log is preserved).
                if raw_log.is_none() && pruner.should_compact(context.estimated_tokens()) {
                    raw_log = Some(context.get_messages());
                }
                let prune_res = pruner
                    .prune(&mut context, Some(&summarizer), &cancel_token)
                    .await;
                if prune_res.checkpoint {
                    emitter
                        .emit(AgentEvent::ContextCompacted {
                            turn: Some(tracker.current_turn()),
                            tokens_before: prune_res.tokens_before,
                            tokens_after: prune_res.tokens_after,
                        })
                        .await;
                }
                if prune_res.pruned {
                    debug!(
                        agent_id = %agent_id,
                        tokens_before = prune_res.tokens_before,
                        tokens_after = prune_res.tokens_after,
                        messages_removed = prune_res.messages_removed,
                        checkpoint = prune_res.checkpoint,
                        emergency = prune_res.emergency,
                        "Context pruned"
                    );
                }

                if let Some(budget) = config.max_tokens_budget {
                    if context.estimated_tokens() > budget {
                        warn!(
                            agent_id = %agent_id,
                            estimated = context.estimated_tokens(),
                            budget = budget,
                            "Token budget exceeded limit"
                        );
                        loop_finish_reason = FinishReason::BudgetExceeded;
                        break;
                    }
                }

                let turn = tracker.next_turn();
                let turn_start_time = Instant::now();
                let now_ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;

                info!(agent_id = %agent_id, turn = turn, estimated_tokens = context.estimated_tokens(), "Turn started");
                emitter
                    .emit(AgentEvent::TurnStart {
                        turn,
                        timestamp: now_ts,
                    })
                    .await;

                // Splice queued input into the transcript before the request is
                // built. It becomes ordinary user messages, so persistence and
                // compaction treat it exactly like something the user typed.
                //
                // Refreshing here (not only at the previous turn's tail) is what
                // catches input queued while tools were executing, and it is
                // guarded because `one-at-a-time` must not deliver two messages
                // in one turn.
                if pending_messages.is_empty() {
                    pending_messages.extend(
                        steer_queues
                            .steering
                            .drain()
                            .into_iter()
                            .map(|m| (m, QueueBehavior::Steer)),
                    );
                }
                for (message, behavior) in std::mem::take(&mut pending_messages) {
                    let text = message.content_str().unwrap_or_default().to_string();
                    let image_count = message.image_count();
                    info!(
                        agent_id = %agent_id,
                        turn = turn,
                        behavior = behavior.label(),
                        chars = text.chars().count(),
                        images = image_count,
                        "Queued message accepted"
                    );
                    context.push(message.clone());
                    if let Some(log) = raw_log.as_mut() {
                        log.push(message);
                    }
                    emitter
                        .emit(AgentEvent::SteerAccepted {
                            turn,
                            behavior: behavior.label().to_string(),
                            message: text,
                            image_count,
                        })
                        .await;
                }

                let max_overflow_retries = 2;
                let mut retry_count = 0;
                let max_stream_retries = config.max_stream_retries;
                let mut stream_retry_count = 0;

                let mut assistant_content = String::new();
                let mut reasoning_content = String::new();
                let mut accumulated_content = String::new();
                let mut had_tool_chunks;
                let mut continuation_prompts_injected = 0;
                let mut completed_chunk = None;

                loop {
                    let request_opts = ChatRequestOptions {
                        messages: context.get_messages(),
                        tools: tool_registry.get_definitions(),
                        model: Some(config.model.clone()),
                        temperature: config.temperature,
                        top_p: config.top_p,
                        max_tokens: config.max_completion_tokens,
                        thinking_level: config.thinking_level.clone(),
                        cache_retention: None, // normal turns benefit from cache writes
                        // Prompt-cache routing affinity: bind every request of
                        // this run to the conversation's cache shard.
                        session_id: config.session_id.clone(),
                    };

                    let stream_res = llm_client
                        .stream_chat(request_opts, cancel_token.clone())
                        .await;
                    let mut stream_rx = match stream_res {
                        Ok(rx) => rx,
                        Err(err) => {
                            if !cancel_token.is_cancelled()
                                && retry_count < max_overflow_retries
                                && crate::pruning::is_context_overflow_error(&err)
                            {
                                retry_count += 1;
                                let detected_limit =
                                    crate::pruning::extract_context_overflow_limit(&err)
                                        .unwrap_or_else(|| {
                                            (pruner.max_tokens() as f32 * 0.6) as usize
                                        });
                                warn!(
                                    agent_id = %agent_id,
                                    turn = turn,
                                    detected_limit = detected_limit,
                                    retry = retry_count,
                                    "Context overflow detected on chat initiation, auto-healing with compaction and retry"
                                );
                                pruner.update_max_tokens(detected_limit);
                                let _ = pruner
                                    .prune(&mut context, Some(&summarizer), &cancel_token)
                                    .await;
                                continue;
                            }

                            if !cancel_token.is_cancelled()
                                && stream_retry_count < max_stream_retries
                            {
                                stream_retry_count += 1;
                                warn!(
                                    agent_id = %agent_id,
                                    turn = turn,
                                    retry = stream_retry_count,
                                    error = %err,
                                    "Network handshake failed, retrying stream connection"
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(
                                    500 * (1 << (stream_retry_count - 1)),
                                ))
                                .await;
                                continue;
                            }

                            error!(agent_id = %agent_id, turn = turn, error = %err, "Failed to initiate stream chat");
                            emitter
                                .emit(AgentEvent::Error {
                                    turn: Some(turn),
                                    message: err.clone(),
                                    recoverable: false,
                                })
                                .await;
                            break;
                        }
                    };

                    assistant_content.clear();
                    reasoning_content.clear();
                    had_tool_chunks = false;
                    completed_chunk = None;
                    let mut stream_failed_overflow = None;
                    let mut stream_interrupted_error = None;

                    while let Some(chunk_res) = stream_rx.recv().await {
                        match chunk_res {
                            Ok(LLMStreamChunk::Token(delta)) => {
                                assistant_content.push_str(&delta);
                                emitter.emit(AgentEvent::TokenDelta { turn, delta }).await;
                            }
                            Ok(LLMStreamChunk::ReasoningToken(delta)) => {
                                reasoning_content.push_str(&delta);
                                emitter
                                    .emit(AgentEvent::ReasoningDelta { turn, delta })
                                    .await;
                            }
                            Ok(LLMStreamChunk::ToolCallChunk(tc_delta)) => {
                                had_tool_chunks = true;
                                emitter
                                    .emit(AgentEvent::ToolCallChunk {
                                        turn,
                                        index: tc_delta.index,
                                        id: tc_delta.id,
                                        name: tc_delta.name,
                                        arguments_delta: tc_delta.arguments_delta,
                                    })
                                    .await;
                            }
                            Ok(LLMStreamChunk::Completed {
                                content,
                                tool_calls,
                                finish_reason,
                                prompt_tokens,
                                completion_tokens,
                                cached_tokens,
                                cache_write_tokens,
                                reasoning_tokens,
                            }) => {
                                completed_chunk = Some((
                                    content,
                                    tool_calls,
                                    finish_reason,
                                    prompt_tokens,
                                    completion_tokens,
                                    cached_tokens,
                                    cache_write_tokens,
                                    reasoning_tokens,
                                ));
                            }
                            Err(stream_err) => {
                                if !cancel_token.is_cancelled()
                                    && crate::pruning::is_context_overflow_error(&stream_err)
                                {
                                    stream_failed_overflow = Some(stream_err);
                                } else if cancel_token.is_cancelled() {
                                    info!(agent_id = %agent_id, turn = turn, "Stream cancelled by user");
                                } else {
                                    stream_interrupted_error = Some(stream_err);
                                }
                                break;
                            }
                        }
                    }

                    if let Some(overflow_err) = stream_failed_overflow {
                        if !cancel_token.is_cancelled() && retry_count < max_overflow_retries {
                            retry_count += 1;
                            let detected_limit =
                                crate::pruning::extract_context_overflow_limit(&overflow_err)
                                    .unwrap_or_else(|| (pruner.max_tokens() as f32 * 0.6) as usize);
                            warn!(
                                agent_id = %agent_id,
                                turn = turn,
                                detected_limit = detected_limit,
                                retry = retry_count,
                                "Context overflow detected during stream, auto-healing with compaction and retry"
                            );
                            pruner.update_max_tokens(detected_limit);
                            let _ = pruner
                                .prune(&mut context, Some(&summarizer), &cancel_token)
                                .await;
                            continue;
                        }
                    }

                    // Handle mid-stream disconnection with differentiated continuation
                    if let Some(stream_err) = stream_interrupted_error {
                        if !cancel_token.is_cancelled() && stream_retry_count < max_stream_retries {
                            stream_retry_count += 1;

                            if had_tool_chunks {
                                // Scenario B: Tool Call arguments interrupted mid-stream
                                warn!(
                                    agent_id = %agent_id,
                                    turn = turn,
                                    retry = stream_retry_count,
                                    "Tool argument stream interrupted; requesting model to re-issue complete tool call"
                                );
                                context.push(ChatMessage::user(
                                    "[System Telemetry: StreamResilience\n • Action: Stream interrupted while transmitting tool call arguments.\n • Ground Truth: Partial unparsed JSON was discarded.\n • Guidance: Please re-issue your intended tool call with complete parameters.]",
                                ));
                                continuation_prompts_injected += 1;
                                assistant_content.clear();
                                continue;
                            } else if !assistant_content.is_empty() {
                                // Scenario A: Content stream interrupted mid-stream -> Breakpoint Continuation
                                accumulated_content.push_str(&assistant_content);
                                let tail = crate::core::utf8::safe_slice_from(
                                    &accumulated_content,
                                    accumulated_content.len().saturating_sub(60),
                                );
                                warn!(
                                    agent_id = %agent_id,
                                    turn = turn,
                                    retry = stream_retry_count,
                                    accumulated_chars = accumulated_content.len(),
                                    "Content stream interrupted; initiating seamless breakpoint continuation"
                                );
                                context.push(ChatMessage::assistant_text(&accumulated_content));
                                context.push(ChatMessage::user(format!(
                                    "[System Telemetry: StreamResilience\n • Action: Network stream disconnected mid-response.\n • Ground Truth: Preserved {} chars ending at: '{}'\n • Guidance: Please continue seamlessly from that exact position without repeating prior text or greeting.]",
                                    accumulated_content.len(),
                                    tail
                                )));
                                continuation_prompts_injected += 2;
                                assistant_content.clear();
                                continue;
                            } else {
                                // Scenario C: Interrupted during reasoning or before first token -> Clean turn retry
                                warn!(
                                    agent_id = %agent_id,
                                    turn = turn,
                                    retry = stream_retry_count,
                                    "Stream disconnected before content was received; performing clean turn retry"
                                );
                                reasoning_content.clear();
                                assistant_content.clear();
                                continue;
                            }
                        } else {
                            error!(agent_id = %agent_id, turn = turn, error = %stream_err, "Stream execution error, retries exhausted");
                            emitter
                                .emit(AgentEvent::Error {
                                    turn: Some(turn),
                                    message: stream_err,
                                    recoverable: false,
                                })
                                .await;
                        }
                    }

                    break;
                }

                // If continuation prompts were injected to heal the stream, cleanly remove them so history remains pristine
                for _ in 0..continuation_prompts_injected {
                    context.pop();
                }

                if cancel_token.is_cancelled() {
                    cancelled_partial = partial_answer(&assistant_content, &reasoning_content);
                    loop_finish_reason = FinishReason::Cancelled;
                    break;
                }

                let (
                    mut chunk_content,
                    tool_calls,
                    finish_reason,
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    cache_write_tokens,
                    reasoning_tokens,
                ) = match completed_chunk {
                    Some(c) => c,
                    None => {
                        loop_finish_reason = if cancel_token.is_cancelled() {
                            cancelled_partial =
                                partial_answer(&assistant_content, &reasoning_content);
                            FinishReason::Cancelled
                        } else {
                            error!(agent_id = %agent_id, turn = turn, "Turn terminated without completion payload");
                            FinishReason::Error
                        };
                        break;
                    }
                };

                // Merge accumulated partial text from earlier stream attempts if continuation succeeded
                if !accumulated_content.is_empty() {
                    if let Some(new_content) = chunk_content.take() {
                        accumulated_content.push_str(&new_content);
                        chunk_content = Some(accumulated_content);
                    } else {
                        chunk_content = Some(accumulated_content);
                    }
                }

                let answer_content = chunk_content.filter(|c| !c.is_empty()).or_else(|| {
                    if !assistant_content.is_empty() {
                        Some(assistant_content.clone())
                    } else {
                        None
                    }
                });

                let has_tool_calls = !tool_calls.is_empty();
                let turn_duration_ms = turn_start_time.elapsed().as_millis() as u64;

                // Reasoning-token breakdown: prefer the provider-reported subset
                // (`completion_tokens_details.reasoning_tokens` / Gemini `thoughtsTokenCount`);
                // otherwise estimate from the accumulated thinking text (Anthropic and
                // providers without an explicit count). Thinking tokens are part of the
                // output total, mirroring industry billing semantics.
                let effective_reasoning_tokens = reasoning_tokens.or_else(|| {
                    if !reasoning_content.is_empty() {
                        Some(crate::core::token_estimator::estimate_token_count(
                            &reasoning_content,
                        ))
                    } else {
                        None
                    }
                });

                // Fallback estimation if the upstream provider omitted usage counts or sent dummy zeros.
                // The estimate covers thinking text as well, keeping totals consistent
                // with the provider-reported semantics above.
                let effective_completion_tokens = match completion_tokens {
                    Some(ct) if ct > 0 => Some(ct),
                    _ => {
                        let text = answer_content.as_deref().unwrap_or(&assistant_content);
                        let answer_tokens = if !text.is_empty() {
                            Some(crate::core::token_estimator::estimate_token_count(text))
                        } else {
                            None
                        };
                        match (answer_tokens, effective_reasoning_tokens) {
                            (Some(a), Some(r)) => Some(a + r),
                            (Some(a), None) => Some(a),
                            (None, Some(r)) => Some(r),
                            (None, None) => completion_tokens,
                        }
                    }
                };
                let effective_prompt_tokens = match prompt_tokens {
                    Some(pt) if pt > 0 => Some(pt),
                    _ => {
                        let count = context.estimated_tokens();
                        if count > 0 {
                            Some(count)
                        } else {
                            prompt_tokens
                        }
                    }
                };

                let tps = if turn_duration_ms > 0 && effective_completion_tokens.is_some() {
                    effective_completion_tokens
                        .map(|ct| (ct as f64) / (turn_duration_ms as f64 / 1000.0))
                } else {
                    None
                };

                // Prompt-cache warming: refresh the cache entry this turn wrote
                // before its TTL expires. Re-scheduled every turn (replacing the
                // previous snapshot, like pi's per-request cacheWarmer.start).
                if let (Some(session_id), Some(warm)) =
                    (config.session_id.clone(), config.prompt_cache_warm.clone())
                {
                    let cache_engaged =
                        cached_tokens.unwrap_or(0) + cache_write_tokens.unwrap_or(0) > 0;
                    if cache_engaged {
                        crate::cache::warmer::schedule(
                            Arc::clone(&llm_client),
                            crate::cache::warmer::WarmSnapshot {
                                model: config.model.clone(),
                                messages: context.get_messages(),
                                tools: tool_registry.get_definitions(),
                                temperature: config.temperature,
                                top_p: config.top_p,
                                thinking_level: config.thinking_level.clone(),
                                session_id,
                                prompt_tokens: effective_prompt_tokens.unwrap_or(0),
                            },
                            warm,
                        );
                    }
                }

                let turn_stats = TurnStats {
                    turn,
                    prompt_tokens: effective_prompt_tokens,
                    completion_tokens: effective_completion_tokens,
                    cached_tokens,
                    cache_write_tokens,
                    reasoning_tokens: effective_reasoning_tokens,
                    duration_ms: turn_duration_ms,
                    tool_calls_count: tool_calls.len(),
                    tokens_per_second: tps,
                };
                tracker.record_turn_stats(turn_stats.clone());

                let assistant_msg = ChatMessage::Assistant {
                    content: answer_content.clone(),
                    tool_calls: if has_tool_calls {
                        Some(tool_calls.clone())
                    } else {
                        None
                    },
                    refusal: None,
                    name: None,
                };
                context.push(assistant_msg.clone());
                if let Some(log) = raw_log.as_mut() {
                    log.push(assistant_msg);
                }

                info!(
                    agent_id = %agent_id,
                    turn = turn,
                    duration_ms = turn_duration_ms,
                    tool_calls = tool_calls.len(),
                    finish_reason = %finish_reason,
                    "Turn finished"
                );

                emitter
                    .emit(AgentEvent::TurnEnd {
                        turn,
                        finish_reason: finish_reason.clone(),
                        stats: turn_stats,
                    })
                    .await;

                if !has_tool_calls {
                    // The model signalled it is finished, but a queued message
                    // can still keep the run alive for one more turn: steering
                    // first, then follow-ups. Follow-ups deliberately only get
                    // their turn once nothing else is left to do.
                    if pending_messages.is_empty() {
                        pending_messages.extend(
                            steer_queues
                                .steering
                                .drain()
                                .into_iter()
                                .map(|m| (m, QueueBehavior::Steer)),
                        );
                    }
                    if pending_messages.is_empty() {
                        pending_messages.extend(
                            steer_queues
                                .follow_up
                                .drain()
                                .into_iter()
                                .map(|m| (m, QueueBehavior::FollowUp)),
                        );
                    }
                    if pending_messages.is_empty() {
                        info!(agent_id = %agent_id, "LLM concluded the task autonomously (no more tool calls)");
                        final_content = answer_content.or_else(|| {
                            if !reasoning_content.is_empty() {
                                Some(reasoning_content.clone())
                            } else {
                                None
                            }
                        });
                        loop_finish_reason = FinishReason::Done;
                        break;
                    }
                    info!(
                        agent_id = %agent_id,
                        queued = pending_messages.len(),
                        "Continuing the run for a queued message"
                    );
                    continue;
                }

                tracker.set_status(LoopStatus::ExecutingTools);
                status.store(LoopStatus::ExecutingTools.as_u8(), Ordering::Release);

                for tc in &tool_calls {
                    debug!(agent_id = %agent_id, tool = %tc.function.name, id = %tc.id, "Tool call scheduled");
                    emitter
                        .emit(AgentEvent::ToolCallReady {
                            turn,
                            tool_call: tc.clone(),
                        })
                        .await;

                    let arguments = if tc.function.arguments.trim().is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|_| serde_json::json!({}))
                    };
                    emitter
                        .emit(AgentEvent::ToolExecStart {
                            turn,
                            tool_call_id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            arguments,
                        })
                        .await;
                }

                // Cooperative pause checkpoint. Held *before* dispatching tools so
                // an in-flight tool is never interrupted mid-write.
                if pause_gate.is_paused() {
                    info!(agent_id = %agent_id, turn, "Paused at tool boundary; awaiting resume");
                    tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => {}
                        _ = pause_gate.wait_if_paused() => {}
                    }
                    if cancel_token.is_cancelled() {
                        loop_finish_reason = FinishReason::Cancelled;
                        break;
                    }
                    info!(agent_id = %agent_id, turn, "Resumed from pause");
                }

                let ws = config
                    .workspace_dir
                    .clone()
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let has_bash = tool_calls.iter().any(|tc| tc.function.name == "bash");
                let before_git_status = if has_bash {
                    detect_git_status_snapshot(&ws).await
                } else {
                    std::collections::HashSet::new()
                };

                let exec_results = tool_executor
                    .execute_all(
                        &tool_calls,
                        turn,
                        cancel_token.clone(),
                        config.route.clone(),
                        Some(std::time::Duration::from_millis(config.request_timeout_ms)),
                    )
                    .await;

                for executed in exec_results {
                    tracker.record_tool_execution(executed.result.duration_ms);
                    guard.record_tool_result(executed.result.is_error);

                    info!(
                        agent_id = %agent_id,
                        tool = %executed.tool_call.function.name,
                        duration_ms = executed.result.duration_ms,
                        is_error = executed.result.is_error,
                        truncated = executed.result.truncated,
                        "Tool executed"
                    );

                    let mut final_tool_output = executed.result.output.clone();

                    if let Some(repetition_warning) = guard.record_and_check_repetition(
                        &executed.tool_call.function.name,
                        &executed.tool_call.function.arguments,
                    ) {
                        warn!(agent_id = %agent_id, tool = %executed.tool_call.function.name, "Repetitive action pattern detected");
                        final_tool_output =
                            format!("{}\n\n{}", final_tool_output, repetition_warning);
                    }

                    if finish_reason == "length"
                        && executed.result.is_error
                        && executed.result.output.contains("Failed to parse JSON")
                    {
                        warn!(agent_id = %agent_id, "Tool arguments were truncated by length limit");
                        final_tool_output = format!(
                            "{}\n\n[Diagnostic Note: Generation was truncated by token limit. Please perform this operation in smaller chunks.]",
                            final_tool_output
                        );
                    }

                    emitter
                        .emit(AgentEvent::ToolExecResult {
                            turn,
                            tool_call_id: executed.tool_call.id.clone(),
                            name: executed.tool_call.function.name.clone(),
                            result: executed.result.clone(),
                        })
                        .await;

                    // Emit TelemetryNotice if structured telemetry is attached
                    if let Some(ref notice) = executed.result.telemetry {
                        emitter
                            .emit(AgentEvent::TelemetryNotice {
                                turn,
                                tool_call_id: executed.tool_call.id.clone(),
                                layer: notice.layer.clone(),
                                action: notice.action.clone(),
                                ground_truth: notice.ground_truth.clone(),
                                self_healed: notice.self_healed.clone(),
                                guidance: notice.guidance.clone(),
                            })
                            .await;
                    }

                    // Emit FileChange for write_file or bash
                    if executed.tool_call.function.name == "write_file" {
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(
                            &executed.tool_call.function.arguments,
                        ) {
                            if let Some(path_str) = parsed.get("path").and_then(|v| v.as_str()) {
                                let bytes = parsed
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .map(|c| c.len());
                                let action = if executed.result.is_error {
                                    "failed".to_string()
                                } else {
                                    "written".to_string()
                                };
                                emitter
                                    .emit(AgentEvent::FileChange {
                                        turn,
                                        tool_call_id: executed.tool_call.id.clone(),
                                        path: path_str.to_string(),
                                        action,
                                        bytes,
                                        tool_name: "write_file".to_string(),
                                    })
                                    .await;
                            }
                        }
                    } else if executed.tool_call.function.name == "bash" {
                        let after_git_status = detect_git_status_snapshot(&ws).await;
                        for entry in &after_git_status {
                            if !before_git_status.contains(entry) {
                                let file_path = if entry.len() > 3 {
                                    entry[3..].trim().to_string()
                                } else {
                                    entry.clone()
                                };
                                let action = if entry.starts_with("??") {
                                    "created".to_string()
                                } else if entry.starts_with('D') || entry.contains(" D") {
                                    "deleted".to_string()
                                } else {
                                    "modified".to_string()
                                };
                                emitter
                                    .emit(AgentEvent::FileChange {
                                        turn,
                                        tool_call_id: executed.tool_call.id.clone(),
                                        path: file_path,
                                        action,
                                        bytes: None,
                                        tool_name: "bash".to_string(),
                                    })
                                    .await;
                            }
                        }
                    }

                    let tool_msg = ChatMessage::Tool {
                        tool_call_id: executed.tool_call.id,
                        content: final_tool_output,
                        name: Some(executed.tool_call.function.name),
                    };
                    context.push(tool_msg.clone());
                    if let Some(log) = raw_log.as_mut() {
                        log.push(tool_msg);
                    }
                }

                tracker.set_status(LoopStatus::Running);
                status.store(LoopStatus::Running.as_u8(), Ordering::Release);
            }

            // ── Settle the transcript ────────────────────────────────────────
            // A stopped run stops *between* protocol steps: the model may have
            // asked for tools that never ran, or streamed an answer that never
            // became a message. Both leave history that the next request either
            // rejects outright (a tool call with no result) or silently loses (an
            // answer the user already read). Close it out here, before anything
            // observes the result, so the TUI, the daemon and every plugin see
            // the same valid transcript.
            if !matches!(loop_finish_reason, FinishReason::Done) {
                for (tool_call_id, name) in dangling_tool_calls(&context.get_messages()) {
                    warn!(
                        agent_id = %agent_id,
                        tool = %name,
                        "Answering a tool call the run never executed"
                    );
                    let message = ChatMessage::Tool {
                        tool_call_id: tool_call_id.clone(),
                        content: CANCELLED_TOOL_RESULT.to_string(),
                        name: Some(name.clone()),
                    };
                    context.push(message.clone());
                    if let Some(log) = raw_log.as_mut() {
                        log.push(message);
                    }
                    // Emitting matters as much as recording: hosts build their
                    // own history from the event stream, and an unanswered call
                    // there is exactly what would lock the user out next turn.
                    emitter
                        .emit(AgentEvent::ToolExecResult {
                            turn: tracker.current_turn(),
                            tool_call_id,
                            name,
                            result: ToolExecutionResult {
                                output: CANCELLED_TOOL_RESULT.to_string(),
                                is_error: true,
                                truncated: false,
                                original_bytes: CANCELLED_TOOL_RESULT.len(),
                                duration_ms: 0,
                                telemetry: None,
                            },
                        })
                        .await;
                }

                if loop_finish_reason == FinishReason::Cancelled {
                    if let Some(partial) = cancelled_partial.take() {
                        if final_content.is_none() {
                            final_content = Some(partial.clone());
                        }
                        let message = ChatMessage::assistant_text(&partial);
                        context.push(message.clone());
                        if let Some(log) = raw_log.as_mut() {
                            log.push(message);
                        }
                    }
                }
            }

            let terminal = match loop_finish_reason {
                FinishReason::Done => LoopStatus::Completed,
                FinishReason::Cancelled => LoopStatus::Aborted,
                _ => LoopStatus::Failed,
            };
            tracker.set_status(terminal);
            status.store(terminal.as_u8(), Ordering::Release);

            // The run settled: switch any warmer it scheduled to idle economics
            // (15% continuation estimate, 30-minute age cap).
            if let Some(session_id) = config.session_id.as_deref() {
                crate::cache::warmer::mark_idle(session_id);
            }

            let stats = tracker.get_stats();

            info!(
                agent_id = %agent_id,
                total_turns = stats.total_turns,
                total_duration_ms = stats.total_duration_ms,
                total_tool_executions = stats.total_tool_executions,
                finish_reason = ?loop_finish_reason,
                "Loop execution completed"
            );

            if config.scratchpad.auto_cleanup {
                if let Err(err) = scratchpad.cleanup().await {
                    warn!(agent_id = %agent_id, error = %err, "Failed to cleanup scratchpad temporary files");
                }
            }

            emitter
                .emit(AgentEvent::LoopComplete {
                    finish_reason: loop_finish_reason.clone(),
                    final_content: final_content.clone(),
                    stats: stats.clone(),
                })
                .await;

            let run_result = AgentRunResult {
                agent_id,
                final_content,
                messages: context.get_messages(),
                // Present only when a checkpoint compaction occurred: the raw,
                // pre-compaction transcript for hosts that persist history.
                raw_messages: raw_log,
                stats,
                finish_reason: loop_finish_reason,
            };

            let _ = result_tx.send(run_result);
        });
    }
}

struct Emitter {
    agent_id: String,
    dispatcher: AgentEventDispatcher,
    event_sender: mpsc::Sender<ObservedEvent>,
}

impl Emitter {
    async fn emit(&self, event: AgentEvent) {
        let is_micro_delta = matches!(
            event,
            AgentEvent::TokenDelta { .. }
                | AgentEvent::ReasoningDelta { .. }
                | AgentEvent::ToolCallChunk { .. }
        );

        let observed = ObservedEvent {
            agent_id: self.agent_id.clone(),
            event,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        self.dispatcher.emit(observed.clone());

        if is_micro_delta {
            // For streaming micro-deltas, never block the core engine loop
            // indefinitely if the consumer's channel is saturated.
            let _ = self.event_sender.try_send(observed);
        } else {
            // For structural lifecycle events, ensure delivery with a safe fallback deadline.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.event_sender.send(observed),
            )
            .await;
        }
    }
}

fn generate_agent_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("agent_{}_{}", std::process::id(), millis)
}

pub enum ContextInput {
    Text(String),
    Messages(Vec<ChatMessage>),
    Buffer(ContextBuffer),
}

impl From<&str> for ContextInput {
    fn from(s: &str) -> Self {
        ContextInput::Text(s.to_string())
    }
}

impl From<String> for ContextInput {
    fn from(s: String) -> Self {
        ContextInput::Text(s)
    }
}

impl From<Vec<ChatMessage>> for ContextInput {
    fn from(msgs: Vec<ChatMessage>) -> Self {
        ContextInput::Messages(msgs)
    }
}

impl From<ContextBuffer> for ContextInput {
    fn from(ctx: ContextBuffer) -> Self {
        ContextInput::Buffer(ctx)
    }
}

async fn detect_git_status_snapshot(
    workspace: &std::path::Path,
) -> std::collections::HashSet<String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            text.lines()
                .filter_map(|line| {
                    let trimmed = line.trim();
                    if trimmed.len() > 3 {
                        Some(trimmed.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        }
        _ => std::collections::HashSet::new(),
    }
}

/// What the model is told about a tool call a cancel abandoned.
///
/// The model reads this as the tool's output, so it has to *answer* the call
/// rather than read as a crash: the point is that the call did not run, which
/// makes re-issuing it a reasonable next move and retrying the same call an
/// unreasonable one.
const CANCELLED_TOOL_RESULT: &str = "[cancelled by user before execution — the tool did not run]";

/// The best available answer text for a turn that was cut short.
///
/// Mirrors the preference a completed turn uses: the visible answer wins, and
/// reasoning is only the fallback for a turn that never produced one.
fn partial_answer(assistant: &str, reasoning: &str) -> Option<String> {
    if !assistant.trim().is_empty() {
        Some(assistant.to_string())
    } else if !reasoning.trim().is_empty() {
        Some(reasoning.to_string())
    } else {
        None
    }
}

/// Tool calls in the last assistant message that have no matching result.
///
/// Tools run as a batch, so a turn either appends every result or none: the only
/// place an unanswered call can be is the final assistant message. Scanned
/// rather than assumed, so this stays correct if a batch ever becomes partial.
fn dangling_tool_calls(messages: &[ChatMessage]) -> Vec<(String, String)> {
    let Some(index) = messages
        .iter()
        .rposition(|m| matches!(m, ChatMessage::Assistant { .. }))
    else {
        return Vec::new();
    };
    let answered: std::collections::HashSet<String> = messages[index + 1..]
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    match &messages[index] {
        ChatMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } => calls
            .iter()
            .filter(|call| !answered.contains(&call.id))
            .map(|call| (call.id.clone(), call.function.name.clone()))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod cancel_repair_tests {
    use super::*;
    use crate::types::message::ToolCall;

    fn assistant_with_calls(ids: &[&str]) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some("let me look".to_string()),
            tool_calls: Some(
                ids.iter()
                    .map(|id| ToolCall::new_function(*id, "read_file", "{}"))
                    .collect(),
            ),
            refusal: None,
            name: None,
        }
    }

    fn tool(call_id: &str) -> ChatMessage {
        ChatMessage::Tool {
            tool_call_id: call_id.to_string(),
            content: "ok".to_string(),
            name: Some("read_file".to_string()),
        }
    }

    #[test]
    fn unanswered_calls_are_reported() {
        let messages = vec![ChatMessage::user("hi"), assistant_with_calls(&["a", "b"])];
        assert_eq!(
            dangling_tool_calls(&messages),
            vec![
                ("a".to_string(), "read_file".to_string()),
                ("b".to_string(), "read_file".to_string())
            ]
        );
    }

    #[test]
    fn answered_calls_are_left_alone() {
        let messages = vec![
            ChatMessage::user("hi"),
            assistant_with_calls(&["a", "b"]),
            tool("a"),
            tool("b"),
        ];
        assert!(dangling_tool_calls(&messages).is_empty());
    }

    /// A partial batch must not re-answer the calls that did run.
    #[test]
    fn only_the_missing_half_is_reported() {
        let messages = vec![assistant_with_calls(&["a", "b"]), tool("a")];
        assert_eq!(
            dangling_tool_calls(&messages),
            vec![("b".to_string(), "read_file".to_string())]
        );
    }

    /// Results from an earlier turn do not answer a later call with the same shape.
    #[test]
    fn a_plain_answer_has_nothing_to_repair() {
        let messages = vec![
            assistant_with_calls(&["a"]),
            tool("a"),
            ChatMessage::user("and now?"),
            ChatMessage::assistant_text("done"),
        ];
        assert!(dangling_tool_calls(&messages).is_empty());
    }

    #[test]
    fn partial_answer_prefers_the_visible_text() {
        assert_eq!(partial_answer("hi", ""), Some("hi".to_string()));
        assert_eq!(
            partial_answer("  ", "thinking"),
            Some("thinking".to_string())
        );
        assert_eq!(partial_answer("", ""), None);
        assert_eq!(partial_answer("hi", "thinking"), Some("hi".to_string()));
    }
}
