use crate::core::context::ContextBuffer;
use crate::core::state::{AgentStateTracker, LoopStatus};
use crate::loop_engine::hooks::AgentEventDispatcher;
use crate::pruning::strategy::ContextPruner;
use crate::stream::client::{ChatRequestOptions, LLMClient, LLMClientTrait, LLMStreamChunk};
use crate::tools::executor::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::types::config::AgentConfig;
use crate::types::event::{AgentEvent, AgentStats, FinishReason, TurnStats};
use crate::types::message::{ChatMessage, Role};
use crate::types::tool::AgentTool;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct AgentRunResult {
    pub final_content: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub stats: AgentStats,
    pub finish_reason: FinishReason,
}

pub struct AgentLoop {
    config: AgentConfig,
    llm_client: Arc<dyn LLMClientTrait>,
    tool_registry: ToolRegistry,
    tool_executor: ToolExecutor,
    event_dispatcher: AgentEventDispatcher,
}

impl AgentLoop {
    pub fn new(config: AgentConfig) -> Self {
        let llm_client = Arc::new(LLMClient::new(&config));
        let tool_registry = ToolRegistry::new(
            config.max_tool_output_bytes,
            std::time::Duration::from_millis(config.request_timeout_ms),
        );
        let tool_executor = ToolExecutor::new(tool_registry.clone());

        Self {
            config,
            llm_client,
            tool_registry,
            tool_executor,
            event_dispatcher: AgentEventDispatcher::default(),
        }
    }

    pub fn with_custom_client(mut self, client: Arc<dyn LLMClientTrait>) -> Self {
        self.llm_client = client;
        self
    }

    pub fn register_tool(&mut self, tool: Arc<dyn AgentTool>) -> &mut Self {
        self.tool_registry.register(tool);
        self.tool_executor = ToolExecutor::new(self.tool_registry.clone());
        self
    }

    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<AgentEvent> {
        self.event_dispatcher.subscribe()
    }

    /// Run the agent loop until completion and return the final aggregated result
    pub async fn run(
        &self,
        input: impl Into<ContextInput>,
        cancel_token: Option<CancellationToken>,
    ) -> Result<AgentRunResult, String> {
        let token = cancel_token.unwrap_or_default();
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(128);

        let mut stream_rx = self.run_stream(input.into(), event_tx, token.clone()).await?;

        tokio::spawn(async move {
            while let Some(_evt) = event_rx.recv().await {
                // Drain local channel
            }
        });

        stream_rx
            .recv()
            .await
            .ok_or_else(|| "Agent loop terminated without a result".to_string())
    }

    /// Execute the agent loop with fine-grained stream events yielded to the channel
    pub async fn run_stream(
        &self,
        input: ContextInput,
        event_sender: tokio::sync::mpsc::Sender<AgentEvent>,
        cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<AgentRunResult>, String> {
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
                    if ctx.is_empty() || ctx.get_entry(0).map(|e| e.message.role() != Role::System).unwrap_or(true) {
                        ctx.set_system_prompt(sys);
                    }
                }
                ctx
            }
            ContextInput::Buffer(ctx) => ctx,
        };

        let (result_tx, result_rx) = tokio::sync::mpsc::channel(1);
        let config = self.config.clone();
        let llm_client = self.llm_client.clone();
        let tool_registry = self.tool_registry.clone();
        let tool_executor = self.tool_executor.clone();
        let dispatcher = self.event_dispatcher.clone();
        let pruner = ContextPruner::new(config.pruning.clone());

        tokio::spawn(async move {
            let mut tracker = AgentStateTracker::new();
            tracker.set_status(LoopStatus::Running);

            let mut final_content = None;
            let loop_finish_reason: FinishReason;

            let emit = |event: AgentEvent| {
                dispatcher.emit(event.clone());
                let _ = event_sender.try_send(event);
            };

            // Main Loop: Driven autonomously by LLM decisions
            loop {
                // Check optional max_turns safeguard
                if let Some(max) = config.max_turns {
                    if tracker.current_turn() >= max {
                        loop_finish_reason = FinishReason::MaxTurnsExceeded;
                        break;
                    }
                }

                // Check cancellation
                if cancel_token.is_cancelled() {
                    loop_finish_reason = FinishReason::Cancelled;
                    break;
                }

                // 1. Context Pruning Check & Token Budget Guard
                pruner.prune(&mut context);

                if let Some(budget) = config.max_tokens_budget {
                    if context.estimated_tokens() > budget {
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

                emit(AgentEvent::TurnStart {
                    turn,
                    timestamp: now_ts,
                });

                // 2. Dispatch LLM Stream Request
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
                        emit(AgentEvent::Error {
                            turn: Some(turn),
                            message: err.clone(),
                            recoverable: false,
                        });
                        loop_finish_reason = if cancel_token.is_cancelled() {
                            FinishReason::Cancelled
                        } else {
                            FinishReason::Error
                        };
                        break;
                    }
                };

                let mut assistant_content = String::new();
                let mut completed_chunk = None;

                while let Some(chunk_res) = stream_rx.recv().await {
                    match chunk_res {
                        Ok(LLMStreamChunk::Token(delta)) => {
                            assistant_content.push_str(&delta);
                            emit(AgentEvent::TokenDelta {
                                turn,
                                delta,
                            });
                        }
                        Ok(LLMStreamChunk::ToolCallChunk(tc_delta)) => {
                            emit(AgentEvent::ToolCallChunk {
                                turn,
                                index: tc_delta.index,
                                id: tc_delta.id,
                                name: tc_delta.name,
                                arguments_delta: tc_delta.arguments_delta,
                            });
                        }
                        Ok(LLMStreamChunk::Completed {
                            content,
                            tool_calls,
                            finish_reason,
                            prompt_tokens,
                            completion_tokens,
                        }) => {
                            completed_chunk = Some((content, tool_calls, finish_reason, prompt_tokens, completion_tokens));
                        }
                        Err(stream_err) => {
                            emit(AgentEvent::Error {
                                turn: Some(turn),
                                message: stream_err,
                                recoverable: false,
                            });
                            break;
                        }
                    }
                }

                let (chunk_content, tool_calls, finish_reason, prompt_tokens, completion_tokens) = match completed_chunk {
                    Some(c) => c,
                    None => {
                        loop_finish_reason = FinishReason::Error;
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

                // Append assistant message to context
                let assistant_msg = ChatMessage::Assistant {
                    content: chunk_content.clone(),
                    tool_calls: if has_tool_calls { Some(tool_calls.clone()) } else { None },
                    refusal: None,
                    name: None,
                };
                context.push(assistant_msg);

                if let Some(c) = chunk_content {
                    final_content = Some(c);
                }

                emit(AgentEvent::TurnEnd {
                    turn,
                    finish_reason: finish_reason.clone(),
                    stats: turn_stats,
                });

                // =========================================================================
                // 🔑 LLM Autonomous Termination Condition:
                // When LLM does not request tool calls (or signals 'stop'), the task is done.
                // =========================================================================
                if !has_tool_calls || finish_reason == "stop" {
                    loop_finish_reason = FinishReason::Done;
                    break;
                }

                // 3. Execute Tool Calls in Parallel
                tracker.set_status(LoopStatus::ExecutingTools);

                for tc in &tool_calls {
                    emit(AgentEvent::ToolCallReady {
                        turn,
                        tool_call: tc.clone(),
                    });
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

                    emit(AgentEvent::ToolExecResult {
                        turn,
                        tool_call_id: executed.tool_call.id.clone(),
                        name: executed.tool_call.function.name.clone(),
                        result: executed.result.clone(),
                    });

                    // Append tool output to context
                    let tool_msg = ChatMessage::Tool {
                        tool_call_id: executed.tool_call.id,
                        content: executed.result.output,
                        name: Some(executed.tool_call.function.name),
                    };
                    context.push(tool_msg);
                }

                tracker.set_status(LoopStatus::Running);
            }

            tracker.set_status(match loop_finish_reason {
                FinishReason::Done => LoopStatus::Completed,
                FinishReason::Cancelled => LoopStatus::Aborted,
                _ => LoopStatus::Failed,
            });

            let stats = tracker.get_stats();

            emit(AgentEvent::LoopComplete {
                finish_reason: loop_finish_reason.clone(),
                final_content: final_content.clone(),
                stats: stats.clone(),
            });

            let run_result = AgentRunResult {
                final_content,
                messages: context.get_messages(),
                stats,
                finish_reason: loop_finish_reason,
            };

            let _ = result_tx.send(run_result).await;
        });

        Ok(result_rx)
    }
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
