use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use thunder_agent_loop::prelude::*;
use thunder_agent_providers::prelude::*;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::*;

use crate::mock::DaemonMockClient;
use crate::protocol::{DaemonRequest, DaemonResponse};

pub struct DaemonService {
    provider_registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
    store: Arc<FsConversationStore>,
    active_tasks: Arc<Mutex<HashMap<String, CancellationToken>>>,
    stdout: Arc<Mutex<tokio::io::Stdout>>,
    default_workspace: PathBuf,
    script_plugin: Arc<ScriptPlugin>,
}

impl DaemonService {
    pub async fn new(workspace: Option<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let default_workspace = workspace.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        let registry = ProviderRegistry::load_default().await.unwrap_or_default();
        let store_root = FsConversationStore::default_store_root();
        let store = FsConversationStore::new(store_root).await?;
        let script_plugin = Arc::new(ScriptPlugin::new().with_workspace(default_workspace.clone()));

        Ok(Self {
            provider_registry: Arc::new(tokio::sync::RwLock::new(registry)),
            store: Arc::new(store),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
            stdout: Arc::new(Mutex::new(tokio::io::stdout())),
            default_workspace,
            script_plugin,
        })
    }

    /// Safely send a newline-delimited JSON response to stdout
    pub async fn send_response(&self, res: DaemonResponse) {
        write_ndjson(&self.stdout, &res).await;
    }

    /// Cancel all active tasks during shutdown
    pub async fn shutdown(&self) {
        let mut tasks = self.active_tasks.lock().await;
        for (_, token) in tasks.drain() {
            token.cancel();
        }
    }

    /// Dispatch incoming command
    pub async fn handle_request(&self, req: DaemonRequest) {
        match req {
            DaemonRequest::Ping { id } => {
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "pong": true,
                        "version": env!("CARGO_PKG_VERSION"),
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::ListModels { id } => {
                // Dynamically reload from disk on demand so edits to ~/.thunder/models.json are immediately picked up
                if let Ok(fresh) = ProviderRegistry::load_default().await {
                    *self.provider_registry.write().await = fresh;
                }
                let registry = self.provider_registry.read().await;
                let models: Vec<serde_json::Value> = registry
                    .list_available()
                    .into_iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.id,
                            "provider": m.provider,
                            "name": m.name,
                            "selection_id": m.selection_id(),
                            "available": m.available,
                            "reasoning": m.reasoning,
                            "thinking_levels": m.thinking_levels,
                            "default_thinking_level": m.default_thinking_level,
                            "context_window": m.context_window,
                            "max_tokens": m.max_tokens,
                        })
                    })
                    .collect();

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "models": models,
                        "utility_model": registry.utility_model,
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::ListConversations { id } => {
                match self.store.list(&ConversationFilter::default()).await {
                    Ok(list) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: true,
                            data: Some(serde_json::to_value(list).unwrap_or_default()),
                            error: None,
                        })
                        .await;
                    }
                    Err(e) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: false,
                            data: None,
                            error: Some(e.to_string()),
                        })
                        .await;
                    }
                }
            }

            DaemonRequest::GetConversation { id, session_id } => {
                match self.store.load(&session_id).await {
                    Ok(conv) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: true,
                            data: Some(serde_json::to_value(conv).unwrap_or_default()),
                            error: None,
                        })
                        .await;
                    }
                    Err(e) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: false,
                            data: None,
                            error: Some(e.to_string()),
                        })
                        .await;
                    }
                }
            }

            DaemonRequest::RunTask {
                id,
                task_id,
                prompt,
                session_id,
                model,
                use_mock,
                workspace_dir,
                thinking_level,
            } => {
                self.handle_run_task(
                    id,
                    task_id,
                    prompt,
                    session_id,
                    model,
                    use_mock.unwrap_or(false),
                    workspace_dir,
                    thinking_level,
                )
                .await;
            }

            DaemonRequest::CancelTask { id, task_id } => {
                let mut tasks = self.active_tasks.lock().await;
                let cancelled = if let Some(token) = tasks.remove(&task_id) {
                    token.cancel();
                    true
                } else {
                    false
                };

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "task_id": task_id,
                        "cancelled": cancelled
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::ReloadPlugins { id, path } => {
                let p = path.map(PathBuf::from);
                self.script_plugin.reload(p).await;
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "reloaded": true
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::GenerateTitle { id, session_id, force } => {
                // Reload the registry so the latest utilityModel config is honored
                if let Ok(fresh) = ProviderRegistry::load_default().await {
                    *self.provider_registry.write().await = fresh;
                }
                let registry = self.provider_registry.read().await.clone();
                match generate_conversation_title(&self.store, &registry, &session_id, force, None, None).await {
                    Ok(title) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: true,
                            data: Some(serde_json::json!({ "title": title, "source": "auto" })),
                            error: None,
                        })
                        .await;
                    }
                    Err(e) => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: false,
                            data: Some(serde_json::json!({ "error_kind": e.kind })),
                            error: Some(e.to_string()),
                        })
                        .await;
                    }
                }
            }

            DaemonRequest::SetConversationTitle { id, session_id, title } => {
                let trimmed = title.trim().to_string();
                if trimmed.is_empty() {
                    self.send_response(DaemonResponse::Response {
                        id,
                        success: false,
                        data: Some(serde_json::json!({ "error_kind": "invalid_title" })),
                        error: Some("[invalid_title] title must not be empty".to_string()),
                    })
                    .await;
                } else {
                    let final_title = if trimmed.chars().count() > 40 {
                        trimmed.chars().take(40).collect::<String>()
                    } else {
                        trimmed
                    };
                    match self.store.load(&session_id).await {
                        Ok(Some(mut conv)) => {
                            conv.title = Some(final_title);
                            conv.title_source = Some("manual".to_string());
                            conv.updated_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                            match self.store.save(&conv).await {
                                Ok(_) => {
                                    info!(session_id = %session_id, "Conversation title set manually");
                                    self.send_response(DaemonResponse::Response {
                                        id,
                                        success: true,
                                        data: Some(serde_json::json!({ "title": conv.title, "source": "manual" })),
                                        error: None,
                                    })
                                    .await;
                                }
                                Err(e) => {
                                    self.send_response(DaemonResponse::Response {
                                        id,
                                        success: false,
                                        data: Some(serde_json::json!({ "error_kind": "store_error" })),
                                        error: Some(format!("[store_error] failed to persist title: {e}")),
                                    })
                                    .await;
                                }
                            }
                        }
                        Ok(None) => {
                            self.send_response(DaemonResponse::Response {
                                id,
                                success: false,
                                data: Some(serde_json::json!({ "error_kind": "not_found" })),
                                error: Some(format!("[not_found] conversation `{session_id}` not found")),
                            })
                            .await;
                        }
                        Err(e) => {
                            self.send_response(DaemonResponse::Response {
                                id,
                                success: false,
                                data: Some(serde_json::json!({ "error_kind": "store_error" })),
                                error: Some(format!("[store_error] failed to load conversation: {e}")),
                            })
                            .await;
                        }
                    }
                }
            }

            DaemonRequest::GetTrace { id, session_id, task_id } => {
                let trace = load_task_trace(self.store.root(), &session_id, task_id.as_deref()).await;
                let found = trace.is_some();
                self.send_response(DaemonResponse::Response {
                    id,
                    success: found,
                    data: trace,
                    error: if !found { Some("Trace not found".to_string()) } else { None },
                })
                .await;
            }

            DaemonRequest::ListTraces { id, session_id } => {
                let list = list_session_traces(self.store.root(), &session_id).await;
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "traces": list })),
                    error: None,
                })
                .await;
            }
        }
    }

    async fn handle_run_task(
        &self,
        id: Option<String>,
        task_id: String,
        prompt: String,
        session_id: Option<String>,
        model: Option<String>,
        mut use_mock: bool,
        workspace_dir: Option<String>,
        thinking_level: Option<String>,
    ) {
        let effective_session_id = session_id.unwrap_or_else(|| {
            format!("sess_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis())
        });

        // Load or create conversation
        let mut conversation = match self.store.load(&effective_session_id).await {
            Ok(Some(existing)) => existing,
            _ => {
                let mut c = Conversation::new(effective_session_id.clone());
                let title = if prompt.chars().count() > 30 {
                    let truncated: String = prompt.chars().take(30).collect();
                    format!("{truncated}...")
                } else {
                    prompt.clone()
                };
                c = c.with_title(title);
                c
            }
        };

        // Dynamically reload latest providers configuration before resolving models
        if let Ok(fresh) = ProviderRegistry::load_default().await {
            *self.provider_registry.write().await = fresh;
        }

        let registry = self.provider_registry.read().await.clone();
        let available = registry.list_available();

        if available.is_empty() && !use_mock {
            warn!("No available LLM provider credentials found, falling back to Mock mode");
            use_mock = true;
        }

        let chosen_model = model
            .or_else(|| conversation.model.clone())
            .unwrap_or_else(|| {
                available
                    .first()
                    .map(|m| m.selection_id())
                    .unwrap_or_else(|| "openai/gpt-4o".to_string())
            });

        // Workspace is strictly immutable once bound to a conversation:
        let chosen_workspace = if let Some(ref existing_ws) = conversation.workspace {
            // Once bound, workspace cannot be modified!
            existing_ws.clone()
        } else {
            // Brand new conversation: bind the incoming workspace_dir or default workspace
            workspace_dir
                .unwrap_or_else(|| self.default_workspace.to_string_lossy().to_string())
        };

        let chosen_thinking = thinking_level
            .or_else(|| conversation.thinking_level.clone())
            .or_else(|| {
                registry
                    .resolve(&chosen_model)
                    .map(|spec| spec.default_thinking_level.clone())
            });

        // Bind model, workspace, and thinking_level permanently to this conversation
        conversation.model = Some(chosen_model.clone());
        conversation.workspace = Some(chosen_workspace.clone());
        conversation.thinking_level = chosen_thinking.clone();

        conversation.add_user_message(&prompt);
        let _ = self.store.save(&conversation).await;

        let cancel_token = CancellationToken::new();
        self.active_tasks
            .lock()
            .await
            .insert(task_id.clone(), cancel_token.clone());

        // Acknowledge task initiation
        self.send_response(DaemonResponse::Response {
            id,
            success: true,
            data: Some(serde_json::json!({
                "task_id": task_id,
                "session_id": effective_session_id,
                "model": chosen_model,
                "workspace": chosen_workspace,
                "thinking_level": chosen_thinking,
                "use_mock": use_mock
            })),
            error: None,
        })
        .await;

        let store = self.store.clone();
        let active_tasks = self.active_tasks.clone();
        let stdout = self.stdout.clone();
        let ws_dir = PathBuf::from(&chosen_workspace);
        let script_plugin = (*self.script_plugin).clone();

        // Spawn async task runner
        tokio::spawn(async move {
            let mut base_cfg = AgentConfig::new(chosen_model.clone()).with_unlimited_turns();
            base_cfg.request_timeout_ms = 120_000;
            if let Some(ref tl) = chosen_thinking {
                base_cfg.thinking_level = Some(tl.clone());
            }
            if let Some(spec) = registry.resolve(&chosen_model) {
                base_cfg.pruning.max_context_tokens = spec.context_window;
            }

            let root = ThunderRoot::new(base_cfg)
                .with_workspace(ws_dir)
                .with_plugin(ConversationPlugin::new(store.clone()))
                .with_plugin(SkillsPlugin::default())
                .with_plugin(McpPlugin::default())
                .with_plugin(script_plugin)
                .with_provider_registry(registry.clone());

            let custom_client: Option<Arc<dyn LLMClientTrait>> = if use_mock {
                Some(Arc::new(DaemonMockClient))
            } else {
                None
            };

            let options = RootRunOptions {
                session_id: Some(effective_session_id.clone()),
                use_mock,
                custom_client,
                cancellation_token: Some(cancel_token),
                forced_plugins: Some(vec![
                    "conversation".to_string(),
                    "skills".to_string(),
                    "mcp".to_string(),
                    "script_plugin".to_string(),
                ]),
                register_builtins: true,
                thinking_level: chosen_thinking.clone(),
            };

            let context_input = conversation.as_context_input();
            let start_time_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

            match root.execute(context_input, options).await {
                Ok(mut handle) => {
                    let selection = handle.selection.clone();
                    let collected_events = Arc::new(tokio::sync::Mutex::new(Vec::new()));
                    let collected_for_stream = Arc::clone(&collected_events);

                    // Stream fine-grained observed events (tokens, tool execution, etc.)
                    if let Some(mut rx) = handle.take_events() {
                        let tid = task_id.clone();
                        let out = stdout.clone();
                        tokio::spawn(async move {
                            while let Some(event) = rx.recv().await {
                                collected_for_stream.lock().await.push(event.clone());
                                let res = DaemonResponse::ObservedEvent {
                                    task_id: tid.clone(),
                                    event,
                                };
                                write_ndjson(&out, &res).await;
                            }
                        });
                    }

                    match handle.join().await {
                        Ok(res) => {
                            let finish_reason = format!("{:?}", res.run_result.finish_reason);
                            let final_content = res.final_content.clone();

                            // Full persistence of message chain (retains bash executions, write_file and tool results)
                            if !res.run_result.messages.is_empty() {
                                conversation.messages = res.run_result.messages.clone();
                            } else if let Some(ref text) = final_content {
                                conversation.add_assistant_message(Some(text.clone()), None);
                            }
                            let task_tokens = res.run_result.stats.total_prompt_tokens + res.run_result.stats.total_completion_tokens;
                            conversation.stats.total_tokens += task_tokens;
                            conversation.stats.turn_count += res.run_result.stats.total_turns;
                            conversation.stats.tool_calls_count += res.run_result.stats.total_tool_executions;
                            conversation.stats.duration_ms += res.run_result.stats.total_duration_ms;
                            let _ = store.save(&conversation).await;

                            // Auto-generate concise conversation title on the first turn,
                            // or later whenever the title is still a placeholder (self-heal,
                            // e.g. for conversations created before this feature existed).
                            let is_first_turn = conversation.stats.turn_count <= res.run_result.stats.total_turns;
                            if (is_first_turn || conversation.is_title_placeholder()) && !use_mock {
                                let store_clone = store.clone();
                                let reg_clone = registry.clone();
                                let sid_clone = effective_session_id.clone();
                                let prompt_clone = prompt.clone();
                                let final_content_clone = final_content.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = generate_conversation_title(
                                        &store_clone,
                                        &reg_clone,
                                        &sid_clone,
                                        false,
                                        Some(prompt_clone),
                                        final_content_clone,
                                    )
                                    .await
                                    {
                                        warn!(session_id = %sid_clone, error = %e, "Auto title generation failed");
                                    }
                                });
                            }

                            // Save full end-to-end task trace
                            let events = collected_events.lock().await.clone();
                            let finished_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                            let trace_data = serde_json::json!({
                                "task_id": task_id,
                                "session_id": effective_session_id,
                                "model": chosen_model,
                                "workspace_dir": chosen_workspace,
                                "prompt": prompt,
                                "started_at_ms": start_time_ms,
                                "finished_at_ms": finished_at_ms,
                                "duration_ms": res.run_result.stats.total_duration_ms,
                                "finish_reason": finish_reason,
                                "stats": res.run_result.stats,
                                "final_content": final_content,
                                "events": events,
                            });
                            save_task_trace(store.root(), &effective_session_id, &task_id, &trace_data).await;

                            if res.run_result.finish_reason == thunder_agent_loop::types::event::FinishReason::Error {
                                let err_msg = "Thunder agent task ended with FinishReason::Error (see observed error events for details)".to_string();
                                error!(task_id = %task_id, finish_reason = %finish_reason, "Task finished with error");
                                let msg = DaemonResponse::TaskFailed {
                                    task_id: task_id.clone(),
                                    session_id: Some(effective_session_id.clone()),
                                    error: err_msg,
                                };
                                write_ndjson(&stdout, &msg).await;
                            } else {
                                info!(
                                    task_id = %task_id,
                                    finish_reason = %finish_reason,
                                    has_content = final_content.is_some(),
                                    "Task execution finished successfully"
                                );
                                let msg = DaemonResponse::TaskCompleted {
                                    task_id: task_id.clone(),
                                    session_id: effective_session_id.clone(),
                                    final_content,
                                    finish_reason,
                                    active_plugins: selection.active_plugin_ids,
                                };
                                write_ndjson(&stdout, &msg).await;
                            }
                        }
                        Err(err) => {
                            error!(error = %err, "Task execution failed");
                            let msg = DaemonResponse::TaskFailed {
                                task_id: task_id.clone(),
                                session_id: Some(effective_session_id.clone()),
                                error: err.to_string(),
                            };
                            write_ndjson(&stdout, &msg).await;
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "Failed to initialize root execution");
                    let msg = DaemonResponse::TaskFailed {
                        task_id: task_id.clone(),
                        session_id: Some(effective_session_id.clone()),
                        error: e.to_string(),
                    };
                    write_ndjson(&stdout, &msg).await;
                }
            }

            active_tasks.lock().await.remove(&task_id);
        });
    }
}

async fn write_ndjson(stdout: &Arc<Mutex<tokio::io::Stdout>>, res: &DaemonResponse) {
    if let Ok(mut json) = serde_json::to_string(res) {
        json.push('\n');
        let mut o = stdout.lock().await;
        let _ = o.write_all(json.as_bytes()).await;
        let _ = o.flush().await;
    }
}

async fn save_task_trace(
    store_root: &std::path::Path,
    session_id: &str,
    task_id: &str,
    trace_data: &serde_json::Value,
) {
    let trace_dir = store_root.join(session_id).join("traces");
    let _ = tokio::fs::create_dir_all(&trace_dir).await;
    let trace_path = trace_dir.join(format!("{}.json", task_id));
    if let Ok(bytes) = serde_json::to_vec_pretty(trace_data) {
        let _ = tokio::fs::write(&trace_path, bytes).await;
    }
}

async fn load_task_trace(
    store_root: &std::path::Path,
    session_id: &str,
    task_id: Option<&str>,
) -> Option<serde_json::Value> {
    let trace_dir = store_root.join(session_id).join("traces");
    if !trace_dir.exists() {
        return None;
    }

    let target_path = if let Some(tid) = task_id {
        trace_dir.join(format!("{}.json", tid))
    } else {
        // Find latest trace file
        let mut entries = tokio::fs::read_dir(&trace_dir).await.ok()?;
        let mut latest_path = None;
        let mut latest_time = std::time::SystemTime::UNIX_EPOCH;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.extension().map(|ext| ext == "json").unwrap_or(false) {
                if let Ok(meta) = entry.metadata().await {
                    if let Ok(modified) = meta.modified() {
                        if modified > latest_time {
                            latest_time = modified;
                            latest_path = Some(p);
                        }
                    }
                }
            }
        }
        latest_path?
    };

    let content = tokio::fs::read(&target_path).await.ok()?;
    serde_json::from_slice(&content).ok()
}

async fn list_session_traces(
    store_root: &std::path::Path,
    session_id: &str,
) -> Vec<serde_json::Value> {
    let trace_dir = store_root.join(session_id).join("traces");
    let mut results = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&trace_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            if p.extension().map(|ext| ext == "json").unwrap_or(false) {
                if let Ok(content) = tokio::fs::read(&p).await {
                    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&content) {
                        results.push(serde_json::json!({
                            "task_id": val.get("task_id"),
                            "started_at_ms": val.get("started_at_ms"),
                            "finished_at_ms": val.get("finished_at_ms"),
                            "duration_ms": val.get("duration_ms"),
                            "finish_reason": val.get("finish_reason"),
                            "model": val.get("model"),
                            "prompt": val.get("prompt"),
                        }));
                    }
                }
            }
        }
    }
    results.sort_by(|a, b| {
        let ta = a.get("started_at_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        let tb = b.get("started_at_ms").and_then(|v| v.as_u64()).unwrap_or(0);
        tb.cmp(&ta)
    });
    results
}

/// Structured error for conversation title generation, surfaced to the host for diagnosis.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TitleGenError {
    /// Machine-readable kind: no_utility_model | client_error | api_error |
    /// empty_title | store_error | manual_locked | not_found
    pub kind: String,
    /// Human-readable detail (include model id / source error where available)
    pub detail: String,
}

impl TitleGenError {
    fn new(kind: &str, detail: impl Into<String>) -> Self {
        Self { kind: kind.to_string(), detail: detail.into() }
    }
}

impl std::fmt::Display for TitleGenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.kind, self.detail)
    }
}

/// Strip quotes/prefixes from a raw model-generated title.
fn clean_generated_title(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '《' || c == '》' || c == '【' || c == '】' || c == '`')
        .trim_start_matches("Title:")
        .trim_start_matches("标题:")
        .trim_start_matches("标题：")
        .trim()
        .to_string()
}

/// Truncate to a display-safe length (char-boundary safe).
fn clamp_title(cleaned: String) -> String {
    if cleaned.chars().count() > 40 {
        cleaned.chars().take(40).collect()
    } else {
        cleaned
    }
}

fn first_message_text(conv: &Conversation, want_user: bool) -> Option<String> {
    for msg in &conv.messages {
        match (msg, want_user) {
            (ChatMessage::User { content, .. }, true) => return Some(content.clone()),
            (ChatMessage::Assistant { content, .. }, false) => {
                return content.clone().filter(|c| !c.trim().is_empty());
            }
            _ => continue,
        }
    }
    None
}

/// Generate (or regenerate) and persist a conversation title.
/// Synchronous: awaits the utility-model call so the caller receives the outcome.
/// Errors are returned structured and also logged at warn level.
pub async fn generate_conversation_title(
    store: &FsConversationStore,
    registry: &ProviderRegistry,
    session_id: &str,
    force: bool,
    prompt_override: Option<String>,
    assistant_override: Option<String>,
) -> Result<String, TitleGenError> {
    let result = generate_conversation_title_inner(
        store,
        registry,
        session_id,
        force,
        prompt_override,
        assistant_override,
    )
    .await;
    match &result {
        Ok(title) => info!(session_id = %session_id, title = %title, "Conversation title generated"),
        Err(e) => warn!(session_id = %session_id, error = %e, "Conversation title generation failed"),
    }
    result
}

async fn generate_conversation_title_inner(
    store: &FsConversationStore,
    registry: &ProviderRegistry,
    session_id: &str,
    force: bool,
    prompt_override: Option<String>,
    assistant_override: Option<String>,
) -> Result<String, TitleGenError> {
    // Load the freshest conversation state from the store
    let conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return Err(TitleGenError::new("not_found", format!("conversation `{session_id}` not found"))),
        Err(e) => return Err(TitleGenError::new("store_error", format!("failed to load conversation: {e}"))),
    };

    // Never overwrite a manual title unless explicitly forced
    if conv.is_title_manual() && !force {
        return Err(TitleGenError::new(
            "manual_locked",
            "title was set manually; pass force=true to override",
        ));
    }

    let prompt = prompt_override.or_else(|| first_message_text(&conv, true))
        .ok_or_else(|| TitleGenError::new("empty_title", "conversation has no user message to summarize"))?;
    let assistant_text = assistant_override.or_else(|| first_message_text(&conv, false));

    // Resolve the utility model used for background naming
    let utility_spec = match registry.resolve_utility_model() {
        Some(s) if s.available => s.clone(),
        Some(s) => {
            return Err(TitleGenError::new(
                "no_utility_model",
                format!("utility model `{}` is not available (missing API key or base URL)", s.selection_id()),
            ));
        }
        None => {
            return Err(TitleGenError::new(
                "no_utility_model",
                "no utility model configured or heuristically resolvable; set `utilityModel` in models.json",
            ));
        }
    };

    let client = thunder_agent_providers::client_for(&utility_spec, 60_000)
        .map_err(|e| TitleGenError::new("client_error", format!("failed to create client for `{}`: {e}", utility_spec.selection_id())))?;

    let user_excerpt = truncate_chars(&prompt, 300);
    let assistant_excerpt = assistant_text.as_deref().map(|t| truncate_chars(t, 300)).unwrap_or_default();

    let naming_instruction = "You are a concise title generator. Generate a concise, descriptive conversation title (between 4 and 10 Chinese characters or 2 to 6 English words, no punctuation, no quotes, no explanations, no prefix like 'Title:') summarizing the exchange.\n\nUser: ".to_string()
        + &user_excerpt
        + "\nAssistant: "
        + &assistant_excerpt;

    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![ChatMessage::user(naming_instruction)],
        tools: vec![],
        model: Some(utility_spec.id.clone()),
        temperature: Some(0.3),
        top_p: Some(0.9),
        // Generous budget: reasoning models may spend tokens thinking before the answer
        max_tokens: Some(100),
        thinking_level: Some("off".to_string()),
    };

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let mut rx = client
        .stream_chat(options, cancel_token)
        .await
        .map_err(|e| TitleGenError::new("api_error", format!("utility model `{}` request failed: {e}", utility_spec.selection_id())))?;

    let mut generated_title = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk {
            Ok(thunder_agent_loop::stream::client::LLMStreamChunk::Token(token)) => {
                generated_title.push_str(&token);
            }
            Ok(thunder_agent_loop::stream::client::LLMStreamChunk::Completed { content, .. }) => {
                if let Some(c) = content {
                    if generated_title.trim().is_empty() {
                        generated_title = c;
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                return Err(TitleGenError::new(
                    "api_error",
                    format!("utility model `{}` stream error: {e}", utility_spec.selection_id()),
                ));
            }
        }
    }

    let cleaned = clean_generated_title(&generated_title);
    if cleaned.is_empty() {
        return Err(TitleGenError::new(
            "empty_title",
            format!("utility model `{}` returned no usable content (raw output empty after cleaning)", utility_spec.selection_id()),
        ));
    }
    let final_title = clamp_title(cleaned);

    // Re-load to avoid clobbering concurrent writes, then persist
    let mut conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        _ => return Err(TitleGenError::new("store_error", format!("conversation `{session_id}` disappeared during title generation"))),
    };
    conv.title = Some(final_title.clone());
    conv.title_source = Some("auto".to_string());
    conv.updated_at_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    store
        .save(&conv)
        .await
        .map_err(|e| TitleGenError::new("store_error", format!("failed to persist title: {e}")))?;

    Ok(final_title)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn cleans_quotes_and_prefixes() {
        assert_eq!(clean_generated_title("  \"多平台总结\" "), "多平台总结");
        assert_eq!(clean_generated_title("《标题》"), "标题");
        assert_eq!(clean_generated_title("Title: News summary"), "News summary");
        assert_eq!(clean_generated_title("标题：新闻总结"), "新闻总结");
        assert_eq!(clean_generated_title("`git help`"), "git help");
    }

    #[test]
    fn clamps_long_titles_by_chars_not_bytes() {
        assert_eq!(clamp_title("短标题".to_string()), "短标题");
        let long = "a".repeat(50);
        assert_eq!(clamp_title(long).chars().count(), 40);
        // 40 CJK chars are 120 bytes — must not panic on char boundaries
        let cjk = "标".repeat(50);
        assert_eq!(clamp_title(cjk).chars().count(), 40);
    }

    #[test]
    fn detects_placeholder_titles() {
        let mut conv = Conversation::new("s1");
        assert!(conv.is_title_placeholder());
        conv = conv.with_title("[Active Workspace: /Users/luca...");
        assert!(conv.is_title_placeholder());
        conv.title = Some("真正的标题".to_string());
        assert!(!conv.is_title_placeholder());
    }

    #[test]
    fn manual_title_source_is_respected() {
        let mut conv = Conversation::new("s1");
        assert!(!conv.is_title_manual());
        conv.title_source = Some("manual".to_string());
        assert!(conv.is_title_manual());
    }

    #[test]
    fn extracts_first_user_and_assistant_messages() {
        let mut conv = Conversation::new("s1");
        conv.messages.push(ChatMessage::system("sys"));
        conv.messages.push(ChatMessage::user("帮我总结新闻"));
        conv.messages.push(ChatMessage::assistant_text("好的,总结如下"));
        assert_eq!(first_message_text(&conv, true).as_deref(), Some("帮我总结新闻"));
        assert_eq!(first_message_text(&conv, false).as_deref(), Some("好的,总结如下"));
    }
}

