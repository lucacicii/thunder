use crate::cache_warmer::{self, WarmSnapshot};
use crate::context::{ContextBuffer, ContextPruningConfig};
use crate::conversation::now_ms;
use crate::gateway::{KvCacheShield, META_CALL_TOOL, META_DISCOVER_TOOLS};
use crate::llm::{LlmDriver, ToolCallRequest};
use crate::loop_guard::LoopGuard;
use crate::middleware::MiddlewarePipeline;
use crate::pause::PauseGate;
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
    PromptCacheWarmSettings, ThunderEvent, ToolCall, ToolDefinition, ToolExecResultPayload,
    ToolExecutionError, ToolExecutionResult, TurnStats,
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
    pub max_turns: usize,
    pub pruning: ContextPruningConfig,
    pub tool_timeout: Duration,
    pub cache_warm_settings: Option<PromptCacheWarmSettings>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            session_id: "sess-default".to_string(),
            workspace_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            system_prompt: "You are Thunder Agent, a microkernel coding assistant.".to_string(),
            model: "gpt-4o".to_string(),
            max_turns: 16,
            pruning: ContextPruningConfig::default(),
            tool_timeout: Duration::from_secs(30),
            cache_warm_settings: None,
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

        self.context.push(ChatMessage::user(user_prompt));
        let mut system_faults = Vec::new();
        let mut compactions_triggered = 0usize;
        let mut stats = AgentStats::default();
        let cancel = self.cancellation_token.clone();

        for turn in 1..=self.config.max_turns {
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
                return Ok(AgentLoopOutcome {
                    final_response: turn_resp.content,
                    turns_executed: turn,
                    compactions_triggered,
                    system_faults,
                    stats,
                });
            }

            // 5. Execute requested tool calls against the frozen `turn_snapshot` (§1).
            for call in &turn_resp.tool_calls {
                if cancel.is_cancelled() {
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

                if self.loop_guard.is_repetition_limit_exceeded() {
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
                if let Some(warn) = rep_warning {
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

        cache_warmer::mark_idle(&self.config.session_id);
        Err(format!(
            "AgentLoop exceeded maximum turns ({})",
            self.config.max_turns
        ))
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

    fn emit_agent(&self, event: AgentEvent) {
        if let Some(ref sink) = self.event_sink {
            sink.on_agent_event(event);
        }
    }
}
