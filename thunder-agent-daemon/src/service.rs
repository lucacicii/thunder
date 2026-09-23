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
}

impl DaemonService {
    pub async fn new(workspace: Option<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let default_workspace = workspace.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        let registry = ProviderRegistry::load_default().await.unwrap_or_default();
        let store_root = FsConversationStore::default_store_root();
        let store = FsConversationStore::new(store_root).await?;

        Ok(Self {
            provider_registry: Arc::new(tokio::sync::RwLock::new(registry)),
            store: Arc::new(store),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
            stdout: Arc::new(Mutex::new(tokio::io::stdout())),
            default_workspace,
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
                        })
                    })
                    .collect();

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "models": models })),
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
                .with_plugin(ConversationPlugin::with_memory_store())
                .with_plugin(SkillsPlugin::default())
                .with_plugin(McpPlugin::default())
                .with_provider_registry(registry);

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
                ]),
                register_builtins: true,
                thinking_level: chosen_thinking.clone(),
            };

            let context_input = conversation.as_context_input();

            match root.execute(context_input, options).await {
                Ok(mut handle) => {
                    let selection = handle.selection.clone();

                    // Stream fine-grained observed events (tokens, tool execution, etc.)
                    if let Some(mut rx) = handle.take_events() {
                        let tid = task_id.clone();
                        let out = stdout.clone();
                        tokio::spawn(async move {
                            while let Some(event) = rx.recv().await {
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

                            if let Some(ref text) = final_content {
                                conversation.add_assistant_message(Some(text.clone()), None);
                                let _ = store.save(&conversation).await;
                            }

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
