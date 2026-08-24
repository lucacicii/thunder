use crate::core::context::ContextBuffer;
use crate::core::state::{AgentStateTracker, LoopStatus};
use crate::loop_engine::guard::LoopGuard;
use crate::loop_engine::handle::{AgentHandle, RunningGuard};
use crate::loop_engine::hooks::AgentEventDispatcher;
use crate::pruning::strategy::ContextPruner;
use crate::stream::client::{ChatRequestOptions, LLMClient, LLMClientTrait, LLMStreamChunk};
use crate::tools::executor::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::tools::scratchpad::ScratchpadManager;
use crate::types::config::AgentConfig;
use crate::types::error::AgentError;
use crate::types::event::{AgentEvent, AgentStats, FinishReason, ObservedEvent, TurnStats};
use crate::types::message::{ChatMessage, Role};
use crate::types::tool::AgentTool;
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
    pub messages: Vec<ChatMessage>,
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
}

impl AgentLoop {
    pub fn new(config: AgentConfig) -> Self {
        let id = generate_agent_id();
        let scratchpad = ScratchpadManager::new(&id, config.scratchpad.clone());
        let llm_client = Arc::new(LLMClient::new(&config));
        let tool_registry = ToolRegistry::new(
            config.max_tool_output_bytes,
            std::time::Duration::from_millis(config.request_timeout_ms),
        )
        .with_scratchpad(scratchpad.clone());

        let tool_executor = ToolExecutor::new(tool_registry.clone());

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
        }
    }

    /// Assign a stable unit id (used in events, scratchpad isolation, errors).
    /// Recreates the scratchpad so files land under `base_dir/<id>/`.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self.scratchpad = ScratchpadManager::new(&self.id, self.config.scratchpad.clone());
        self.tool_registry.set_scratchpad(self.scratchpad.clone());
        self.tool_executor = ToolExecutor::new(self.tool_registry.clone());
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn status(&self) -> LoopStatus {
        LoopStatus::from_u8(self.status.load(Ordering::Acquire))
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn with_custom_client(mut self, client: Arc<dyn LLMClientTrait>) -> Self {
        self.llm_client = client;
        self
    }

    /// Inject a shared HTTP client (proxy, mTLS, connection pool) for the default LLM transport.
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        self.llm_client = Arc::new(LLMClient::from_client(&self.config, client));
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn AgentTool>) -> &mut Self {
        self.tool_registry.register(tool);
        self.tool_executor = ToolExecutor::new(self.tool_registry.clone());
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

        self.spawn_loop(input.into(), event_tx, result_tx, token.clone());

        Ok(AgentHandle::new(
            self.id.clone(),
            self.status.clone(),
            self.running.clone(),
            token,
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
            tokio::spawn(async move {
                while ev.recv().await.is_some() {}
            });
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
        let pruner = ContextPruner::new(config.pruning.clone());
        let scratchpad = self.scratchpad.clone();
        let agent_id = self.id.clone();
        let status = self.status.clone();
        let running = self.running.clone();

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

            let guard_cfg = &config.loop_guard;
            let mut guard = LoopGuard::new(
                guard_cfg.max_history,
                guard_cfg.repetition_threshold,
                guard_cfg.hard_repetition_limit,
                guard_cfg.max_consecutive_errors,
            );
            let mut final_content = None;
            let loop_finish_reason: FinishReason;

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

                let artifacts_summary = scratchpad.format_artifacts_summary();
                let prune_res = pruner.prune_with_artifacts(&mut context, &artifacts_summary);
                if prune_res.pruned {
                    debug!(
                        agent_id = %agent_id,
                        tokens_before = prune_res.tokens_before,
                        tokens_after = prune_res.tokens_after,
                        messages_removed = prune_res.messages_removed,
                        tools_truncated = prune_res.tool_outputs_truncated,
                        compacted = prune_res.compacted,
                        "Context successfully pruned / compacted"
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
                    .as_secs();

                info!(agent_id = %agent_id, turn = turn, estimated_tokens = context.estimated_tokens(), "Turn started");
                emitter
                    .emit(AgentEvent::TurnStart {
                        turn,
                        timestamp: now_ts,
                    })
                    .await;

                let request_opts = ChatRequestOptions {
                    messages: context.get_messages(),
                    tools: tool_registry.get_definitions(),
                    model: Some(config.model.clone()),
                    temperature: config.temperature,
                    top_p: config.top_p,
                    max_tokens: config.max_completion_tokens,
                };

                let stream_res = llm_client.stream_chat(request_opts, cancel_token.clone()).await;
                let mut stream_rx = match stream_res {
                    Ok(rx) => rx,
                    Err(err) => {
                        error!(agent_id = %agent_id, turn = turn, error = %err, "Failed to initiate stream chat");
                        emitter
                            .emit(AgentEvent::Error {
                                turn: Some(turn),
                                message: err.clone(),
                                recoverable: false,
                            })
                            .await;
                        loop_finish_reason = if cancel_token.is_cancelled() {
                            FinishReason::Cancelled
                        } else {
                            FinishReason::Error
                        };
                        break;
                    }
                };

                let mut assistant_content = String::new();
                let mut reasoning_content = String::new();
                let mut completed_chunk = None;

                while let Some(chunk_res) = stream_rx.recv().await {
                    match chunk_res {
                        Ok(LLMStreamChunk::Token(delta)) => {
                            assistant_content.push_str(&delta);
                            emitter
                                .emit(AgentEvent::TokenDelta { turn, delta })
                                .await;
                        }
                        Ok(LLMStreamChunk::ReasoningToken(delta)) => {
                            reasoning_content.push_str(&delta);
                            emitter
                                .emit(AgentEvent::ReasoningDelta { turn, delta })
                                .await;
                        }
                        Ok(LLMStreamChunk::ToolCallChunk(tc_delta)) => {
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
                        }) => {
                            completed_chunk =
                                Some((content, tool_calls, finish_reason, prompt_tokens, completion_tokens));
                        }
                        Err(stream_err) => {
                            if cancel_token.is_cancelled() {
                                info!(agent_id = %agent_id, turn = turn, "Stream cancelled by user");
                            } else {
                                error!(agent_id = %agent_id, turn = turn, error = %stream_err, "Stream execution error");
                                emitter
                                    .emit(AgentEvent::Error {
                                        turn: Some(turn),
                                        message: stream_err,
                                        recoverable: false,
                                    })
                                    .await;
                            }
                            break;
                        }
                    }
                }

                if cancel_token.is_cancelled() {
                    loop_finish_reason = FinishReason::Cancelled;
                    break;
                }

                let (chunk_content, tool_calls, finish_reason, prompt_tokens, completion_tokens) =
                    match completed_chunk {
                        Some(c) => c,
                        None => {
                            loop_finish_reason = if cancel_token.is_cancelled() {
                                FinishReason::Cancelled
                            } else {
                                error!(agent_id = %agent_id, turn = turn, "Turn terminated without completion payload");
                                FinishReason::Error
                            };
                            break;
                        }
                    };

                let has_tool_calls = !tool_calls.is_empty();
                let turn_duration_ms = turn_start_time.elapsed().as_millis() as u64;

                let turn_stats = TurnStats {
                    turn,
                    prompt_tokens,
                    completion_tokens,
                    duration_ms: turn_duration_ms,
                    tool_calls_count: tool_calls.len(),
                };
                tracker.record_turn_stats(turn_stats.clone());

                let answer_content = chunk_content.filter(|c| !c.is_empty()).or_else(|| {
                    if !assistant_content.is_empty() {
                        Some(assistant_content.clone())
                    } else {
                        None
                    }
                });

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
                context.push(assistant_msg);

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

                let exec_results = tool_executor
                    .execute_all(
                        &tool_calls,
                        turn,
                        cancel_token.clone(),
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
                        final_tool_output = format!("{}\n\n{}", final_tool_output, repetition_warning);
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

                    let tool_msg = ChatMessage::Tool {
                        tool_call_id: executed.tool_call.id,
                        content: final_tool_output,
                        name: Some(executed.tool_call.function.name),
                    };
                    context.push(tool_msg);
                }

                tracker.set_status(LoopStatus::Running);
                status.store(LoopStatus::Running.as_u8(), Ordering::Release);
            }

            let terminal = match loop_finish_reason {
                FinishReason::Done => LoopStatus::Completed,
                FinishReason::Cancelled => LoopStatus::Aborted,
                _ => LoopStatus::Failed,
            };
            tracker.set_status(terminal);
            status.store(terminal.as_u8(), Ordering::Release);

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
        let observed = ObservedEvent {
            agent_id: self.agent_id.clone(),
            event,
        };
        self.dispatcher.emit(observed.clone());
        let _ = self.event_sender.send(observed).await;
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
