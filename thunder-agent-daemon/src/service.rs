use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use thunder_agent_loop::prelude::*;
use thunder_agent_providers::prelude::*;
use thunder_agent_root::prelude::*;
use thunder_conversation::prelude::*;

#[cfg(feature = "testing-mock")]
use crate::mock::DaemonMockClient;
use crate::protocol::{DaemonRequest, DaemonResponse};

/// Host/test seam for resolving the LLM client of a task run.
///
/// Production leaves this unset and models resolve through the provider
/// registry. Tests embed the daemon in-process and inject a fake client here
/// instead of driving mock behaviour through the wire protocol.
pub type ClientFactory = Arc<dyn Fn(&AgentConfig) -> Option<Arc<dyn LLMClientTrait>> + Send + Sync>;

pub struct DaemonService {
    provider_registry: Arc<tokio::sync::RwLock<ProviderRegistry>>,
    store: Arc<FsConversationStore>,
    active_tasks: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Live pause gates per running task.
    active_pauses: Arc<Mutex<HashMap<String, Arc<thunder_agent_loop::core::pause::PauseGate>>>>,
    output_tx: mpsc::Sender<String>,
    concurrency_semaphore: Arc<tokio::sync::Semaphore>,
    default_workspace: PathBuf,
    script_plugin: Arc<ScriptPlugin>,
    /// Optional client factory (tests / embedders). `None` = provider registry.
    client_factory: Option<ClientFactory>,
    /// Pending `ask_user_question` calls, keyed by `task_id:question_id`.
    ///
    /// Dual key on purpose: the daemon multiplexes concurrent tasks, so a bare
    /// question id could cross-wire two tasks' answers.
    pending_questions: Arc<Mutex<HashMap<String, oneshot::Sender<QuestionOutcome>>>>,
    /// Pending `ui_request` dialogs awaiting an `answer_ui`, keyed by the
    /// server-issued request id.
    pending_ui: crate::ui::PendingUiRequests,
    /// The host's user-interaction surface, shared with plugins and with the
    /// permission approval gate.
    host_ui: Arc<crate::ui::DaemonHostUi>,
    /// Per-session approval state: the live mode plus remembered rules.
    ///
    /// Session-scoped on purpose. A fresh session must not inherit a previous
    /// conversation's "always allow", and a mode switch has to outlive the turn
    /// that issued it.
    session_policies: Arc<tokio::sync::RwLock<HashMap<String, Arc<SessionPolicy>>>>,
}

/// Resolution of a pending question.
#[derive(Debug)]
pub enum QuestionOutcome {
    Answered(serde_json::Value),
    Cancelled,
}

/// Kick off a detached, best-effort thinking-level probe.
///
/// Deliberately fire-and-forget: a slow or unreachable provider must not delay
/// daemon startup, and a failure is harmless (the heuristics in the catalog
/// provide sane levels until a later run persists real ones).
fn spawn_thinking_level_probe() {
    tokio::spawn(async {
        let mut registry = match ProviderRegistry::load_default().await {
            Ok(r) => r,
            Err(err) => {
                warn!(error = %err, "Skipping thinking-level probe: registry load failed");
                return;
            }
        };
        registry.probe_and_persist_unprobed_models().await;
    });
}

impl DaemonService {
    /// Inject a client factory (tests/embedders). Without it, clients resolve
    /// from the provider registry exactly as in production.
    ///
    /// Currently only exercised by out-of-tree embedders and future in-process
    /// tests; the daemon binary itself never calls it.
    #[allow(dead_code)]
    pub fn with_client_factory(mut self, factory: ClientFactory) -> Self {
        self.client_factory = Some(factory);
        self
    }

    pub async fn new(workspace: Option<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let default_workspace = workspace
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let registry = ProviderRegistry::load_default().await.unwrap_or_default();
        // One-shot thinking-level enrichment, off the request path. Findings are
        // persisted to models.json, so the per-request reloads below stay pure
        // reads and never block a `list_models` / `generate_title` call on HTTP.
        spawn_thinking_level_probe();
        let store_root = FsConversationStore::default_store_root();
        let store = FsConversationStore::new(store_root).await?;
        let script_plugin = Arc::new(ScriptPlugin::new().with_workspace(default_workspace.clone()));

        let max_tasks = std::env::var("THUNDER_MAX_CONCURRENT_TASKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(8)
            .max(1);
        let concurrency_semaphore = Arc::new(tokio::sync::Semaphore::new(max_tasks));

        let (output_tx, mut output_rx) = mpsc::channel::<String>(1024);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut stdout = tokio::io::stdout();
            let mut buf = Vec::with_capacity(8192);
            while let Some(line) = output_rx.recv().await {
                buf.clear();
                buf.extend_from_slice(line.as_bytes());
                while let Ok(next) = output_rx.try_recv() {
                    buf.extend_from_slice(next.as_bytes());
                    if buf.len() >= 64 * 1024 {
                        break;
                    }
                }
                if stdout.write_all(&buf).await.is_err() {
                    break;
                }
                let _ = stdout.flush().await;
            }
        });

        let pending_ui: crate::ui::PendingUiRequests = Arc::new(Mutex::new(HashMap::new()));
        let host_ui = crate::ui::DaemonHostUi::new(output_tx.clone(), Arc::clone(&pending_ui));

        Ok(Self {
            provider_registry: Arc::new(tokio::sync::RwLock::new(registry)),
            store: Arc::new(store),
            active_tasks: Arc::new(Mutex::new(HashMap::new())),
            active_pauses: Arc::new(Mutex::new(HashMap::new())),
            output_tx,
            concurrency_semaphore,
            default_workspace,
            script_plugin,
            client_factory: None,
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
            pending_ui,
            host_ui,
            session_policies: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        })
    }

    /// The approval policy for a session, created on first use.
    ///
    /// The tier passed to `SessionPolicy::new` is only a seed: every run writes
    /// its own via `set_tier`, because a role can differ between runs of one
    /// session. Until then the policy enforces the widest tier, so a request
    /// arriving before any run cannot be judged too narrowly.
    async fn session_policy(
        &self,
        session_id: &str,
        initial: PermissionMode,
    ) -> Arc<SessionPolicy> {
        if let Some(existing) = self.session_policies.read().await.get(session_id) {
            return Arc::clone(existing);
        }
        let mut guard = self.session_policies.write().await;
        Arc::clone(
            guard
                .entry(session_id.to_string())
                .or_insert_with(|| SessionPolicy::new(Permission::Bash, initial)),
        )
    }

    /// The host UI surface, for handing to plugins and to the (future)
    /// permission approval gate. Scoped per run by the task runner.
    #[allow(dead_code)]
    pub fn host_ui(&self) -> Arc<crate::ui::DaemonHostUi> {
        Arc::clone(&self.host_ui)
    }

    /// Safely send a newline-delimited JSON response to stdout
    pub async fn send_response(&self, res: DaemonResponse) {
        write_ndjson(&self.output_tx, &res).await;
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
                extra_workspace_dirs,
                thinking_level,
                role,
                mode,
            } => {
                self.handle_run_task(
                    id,
                    task_id,
                    prompt,
                    session_id,
                    model,
                    use_mock.unwrap_or(false),
                    workspace_dir,
                    extra_workspace_dirs,
                    thinking_level,
                    role,
                    mode,
                )
                .await;
            }

            DaemonRequest::ListRoles { id, workspace_dir } => {
                let ws = workspace_dir.map(PathBuf::from);
                let registry = RoleRegistry::load_default(ws.as_deref()).await;
                let roles: Vec<serde_json::Value> = registry
                    .list_enabled()
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "id": r.id,
                            "name": r.display_name(),
                            "aliases": r.aliases,
                            "description": r.description,
                            "permission": r.permission.as_str(),
                            "mode": r.mode.map(|m| m.as_str()),
                            "persona": r.persona.as_text(),
                            "model": r.model,
                            "thinking_level": r.thinking_level,
                            "ask_user": r.ask_user,
                            "exit_gate": r.exit_gate,
                        })
                    })
                    .collect();

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "roles": roles })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::PauseTask { id, task_id } => {
                let paused = match self.active_pauses.lock().await.get(&task_id) {
                    Some(gate) => {
                        gate.pause();
                        true
                    }
                    None => false,
                };
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "task_id": task_id, "paused": paused })),
                    error: None,
                })
                .await;
                if paused {
                    let _ = self
                        .send_response(DaemonResponse::TaskPaused {
                            task_id,
                            session_id: None,
                            reason: "Paused by user; will hold at the next tool boundary"
                                .to_string(),
                        })
                        .await;
                }
            }

            DaemonRequest::ResumeTask { id, task_id } => {
                let resumed = match self.active_pauses.lock().await.get(&task_id) {
                    Some(gate) => {
                        gate.resume();
                        true
                    }
                    None => false,
                };
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({ "task_id": task_id, "resumed": resumed })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::AnswerQuestion {
                id,
                question_id,
                answers,
                cancelled,
            } => {
                // question_id is globally unique (`task_id:qN`), so no task lookup
                // is needed here.
                let outcome = if cancelled {
                    QuestionOutcome::Cancelled
                } else {
                    QuestionOutcome::Answered(answers)
                };
                let delivered = self
                    .pending_questions
                    .lock()
                    .await
                    .remove(&question_id)
                    .map(|tx| tx.send(outcome).is_ok())
                    .unwrap_or(false);

                if !delivered {
                    warn!(question_id = %question_id, "Answer for unknown/expired question");
                }

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "question_id": question_id,
                        "delivered": delivered
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::AnswerUi {
                id,
                request_id,
                value,
                confirmed,
                cancelled,
            } => {
                // An explicit answer wins over cancellation only when the panel
                // actually sent one; `confirm` returning false and "dismissed"
                // stay distinguishable so a caller can log the difference.
                let response = if cancelled {
                    UiResponse::Cancelled
                } else if let Some(confirmed) = confirmed {
                    UiResponse::Confirmed { confirmed }
                } else {
                    UiResponse::Value { value }
                };
                let delivered =
                    crate::ui::deliver_ui_response(&self.pending_ui, &request_id, response).await;

                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "request_id": request_id,
                        "delivered": delivered
                    })),
                    error: None,
                })
                .await;
            }

            DaemonRequest::SetPermissionMode {
                id,
                session_id,
                mode,
            } => {
                match PermissionMode::parse(&mode) {
                    Some(parsed) => {
                        // Reuse the session's policy when one exists so a switch
                        // applies to a task that is already running; otherwise
                        // create it so the next run inherits the choice.
                        let policy = self
                            .session_policy(&session_id, PermissionMode::default())
                            .await;
                        policy.set_mode(parsed).await;
                        info!(session_id = %session_id, mode = parsed.as_str(), "Approval mode switched");
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: true,
                            data: Some(serde_json::json!({
                                "session_id": session_id,
                                "mode": parsed.as_str(),
                            })),
                            error: None,
                        })
                        .await;
                    }
                    None => {
                        self.send_response(DaemonResponse::Response {
                            id,
                            success: false,
                            data: Some(serde_json::json!({ "valid_modes":
                                PermissionMode::ALL.map(|m| m.as_str())
                            })),
                            error: Some(format!("unknown mode: {mode}")),
                        })
                        .await;
                    }
                }
            }

            DaemonRequest::GetPermissionState { id, session_id } => {
                let policy = self
                    .session_policy(&session_id, PermissionMode::default())
                    .await;
                let mode = policy.mode().await;
                self.send_response(DaemonResponse::Response {
                    id,
                    success: true,
                    data: Some(serde_json::json!({
                        "session_id": session_id,
                        "mode": mode.as_str(),
                        "rules": policy.rules().await,
                    })),
                    error: None,
                })
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
                // The session-locked plugin selection is now stale: a session
                // that cached its set before this reload would keep spawning the
                // old toolset. Clear it so the next run re-selects.
                invalidate_all_session_selections();
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

            DaemonRequest::GenerateTitle {
                id,
                session_id,
                force,
            } => {
                // Reload the registry so the latest utilityModel config is honored
                if let Ok(fresh) = ProviderRegistry::load_default().await {
                    *self.provider_registry.write().await = fresh;
                }
                let registry = self.provider_registry.read().await.clone();
                match generate_conversation_title(
                    &self.store,
                    &registry,
                    &session_id,
                    force,
                    None,
                    None,
                )
                .await
                {
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

            DaemonRequest::SetConversationTitle {
                id,
                session_id,
                title,
            } => {
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
                            conv.updated_at_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis()
                                as u64;
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
                                        data: Some(
                                            serde_json::json!({ "error_kind": "store_error" }),
                                        ),
                                        error: Some(format!(
                                            "[store_error] failed to persist title: {e}"
                                        )),
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
                                error: Some(format!(
                                    "[not_found] conversation `{session_id}` not found"
                                )),
                            })
                            .await;
                        }
                        Err(e) => {
                            self.send_response(DaemonResponse::Response {
                                id,
                                success: false,
                                data: Some(serde_json::json!({ "error_kind": "store_error" })),
                                error: Some(format!(
                                    "[store_error] failed to load conversation: {e}"
                                )),
                            })
                            .await;
                        }
                    }
                }
            }

            DaemonRequest::GetTrace {
                id,
                session_id,
                task_id,
            } => {
                let trace =
                    load_task_trace(self.store.root(), &session_id, task_id.as_deref()).await;
                let found = trace.is_some();
                self.send_response(DaemonResponse::Response {
                    id,
                    success: found,
                    data: trace,
                    error: if !found {
                        Some("Trace not found".to_string())
                    } else {
                        None
                    },
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
        use_mock: bool,
        workspace_dir: Option<String>,
        extra_workspace_dirs: Option<Vec<String>>,
        thinking_level: Option<String>,
        role_id: Option<String>,
        mode: Option<String>,
    ) {
        // Reject mock-mode requests early when this build has no mock compiled
        // in: a release daemon must never imply it produced real model output.
        #[cfg(not(feature = "testing-mock"))]
        if use_mock {
            self.send_response(DaemonResponse::Response {
                id,
                success: false,
                data: None,
                error: Some(
                    "mock mode is not compiled into this build; rebuild with \
                     `--features thunder-agent-daemon/testing-mock` or configure a real \
                     provider in ~/.thunder/models.json + auth.json"
                        .to_string(),
                ),
            })
            .await;
            return;
        }

        let cancel_token = CancellationToken::new();
        let pause_gate = Arc::new(thunder_agent_loop::core::pause::PauseGate::new());

        // Acquire concurrency permit up-front to bound simultaneous active tasks
        let permit = match Arc::clone(&self.concurrency_semaphore).try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.send_response(DaemonResponse::Response {
                    id,
                    success: false,
                    data: None,
                    error: Some("Server busy: maximum concurrent task limit reached".to_string()),
                })
                .await;
                return;
            }
        };

        // Reject re-entrant task submission if the task_id is already actively running.
        // Prevents task zombie token overwrite, accidental double-clicks, and store race conditions.
        {
            let mut tasks = self.active_tasks.lock().await;
            if tasks.contains_key(&task_id) {
                self.send_response(DaemonResponse::Response {
                    id,
                    success: false,
                    data: None,
                    error: Some(format!("Task '{task_id}' is already running (conflict)")),
                })
                .await;
                return;
            }
            tasks.insert(task_id.clone(), cancel_token.clone());
        }

        self.active_pauses
            .lock()
            .await
            .insert(task_id.clone(), Arc::clone(&pause_gate));

        let effective_session_id = session_id.unwrap_or_else(|| {
            format!(
                "sess_{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            )
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
        let has_valid_credentials = available.iter().any(|m| m.available);

        if !has_valid_credentials {
            // No silent mock fallback: a daemon that answers with canned text
            // while the operator believes it is calling a real model is worse
            // than an explicit failure.
            warn!("No available LLM provider credentials found");
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
            workspace_dir.unwrap_or_else(|| self.default_workspace.to_string_lossy().to_string())
        };

        // Shared roots follow a merge policy instead of first-bind-wins: hosts
        // (e.g. the panel) send the full referenced-repository list on every
        // run, so a repo added to the task later becomes writable mid-task.
        let mut chosen_shared_roots = conversation.shared_roots.clone();
        for dir in extra_workspace_dirs.into_iter().flatten() {
            let dir = dir.trim().to_string();
            if !dir.is_empty() && !chosen_shared_roots.contains(&dir) {
                chosen_shared_roots.push(dir);
            }
        }

        let chosen_thinking = thinking_level
            .or_else(|| conversation.thinking_level.clone())
            .or_else(|| {
                registry
                    .resolve(&chosen_model)
                    .map(|spec| spec.default_thinking_level.clone())
            });

        // Resolve the requested role against global + workspace scopes.
        // Permission is the load-bearing part: it decides which built-in tools
        // (and plugin RPCs) exist at all for this run.
        let role_registry =
            RoleRegistry::load_default(Some(std::path::Path::new(&chosen_workspace))).await;
        // Single-sourced role→permission derivation (see `RoleRegistry::resolve_for_run`).
        let (chosen_role, chosen_permission) = role_registry.resolve_for_run(role_id.as_deref());
        if role_id.is_some() && chosen_role.is_none() {
            warn!(role = ?role_id, "Requested role not found or disabled; running unconstrained");
        }
        if let Some(role) = &chosen_role {
            info!(
                role = %role.display_name(),
                permission = %chosen_permission.describe(),
                "Role resolved for task"
            );
        }

        // Approval mode precedence: explicit request → the session's current mode
        // (so a mid-session `set_permission_mode` survives the next turn) → the
        // role's own mode → `ask`.
        let requested_mode = match mode.as_deref() {
            Some(raw) => match PermissionMode::parse(raw) {
                Some(m) => Some(m),
                None => {
                    warn!(mode = %raw, "Unrecognised mode requested; keeping the session's current mode");
                    None
                }
            },
            None => None,
        };
        let policy = self
            .session_policy(&effective_session_id, PermissionMode::default())
            .await;
        let effective_mode = match requested_mode {
            Some(m) => m,
            // No explicit request: keep whatever the session is already in, so a
            // mid-session `set_permission_mode` is not silently reverted here.
            None => policy.mode().await,
        };
        policy.set_mode(effective_mode).await;
        info!(
            session_id = %effective_session_id,
            mode = effective_mode.as_str(),
            "Approval mode for task"
        );

        // Bind model, workspace, and thinking_level permanently to this conversation
        conversation.model = Some(chosen_model.clone());
        conversation.workspace = Some(chosen_workspace.clone());
        conversation.shared_roots = chosen_shared_roots.clone();
        conversation.thinking_level = chosen_thinking.clone();

        conversation.add_user_message(&prompt);
        let _ = self.store.save(&conversation).await;

        // Acknowledge task initiation
        let response_data = serde_json::json!({
            "task_id": task_id,
            "session_id": effective_session_id,
            "model": chosen_model,
            "workspace": chosen_workspace,
            "shared_roots": chosen_shared_roots,
            "thinking_level": chosen_thinking,
            "use_mock": use_mock
        });

        self.send_response(DaemonResponse::Response {
            id,
            success: true,
            data: Some(response_data),
            error: None,
        })
        .await;

        let store = self.store.clone();
        let active_tasks = self.active_tasks.clone();
        let active_pauses = self.active_pauses.clone();
        let output_tx = self.output_tx.clone();
        let client_factory = self.client_factory.clone();
        let ws_dir = PathBuf::from(&chosen_workspace);
        // MCP tools must stay reachable for natural-language prompts (the keyword
        // heuristic would never match them), but only when the workspace actually
        // configures servers — otherwise forcing the plugin is pure overhead.
        let workspace_has_mcp_config =
            ws_dir.join("mcp_servers.json").exists() || ws_dir.join(".mcp.json").exists();
        // A workspace with no plugin files must not pay for a Node sidecar, and
        // one with plugin files must actually get them.
        let workspace_has_ts_plugins = has_ts_plugins(Some(&ws_dir));
        let script_plugin = (*self.script_plugin).clone();
        let pending_questions = self.pending_questions.clone();
        let pause_gate_for_run = Arc::clone(&pause_gate);
        let host_ui = Arc::clone(&self.host_ui);
        let run_task_id = task_id.clone();
        let run_session_id = effective_session_id.clone();
        let run_policy = Arc::clone(&policy);
        let run_mode = effective_mode;

        // Spawn async task runner
        tokio::spawn(async move {
            let _permit = permit;
            let mut base_cfg = AgentConfig::new(chosen_model.clone()).with_unlimited_turns();
            base_cfg.request_timeout_ms = 120_000;
            // Prompt-cache routing affinity: every request of this conversation
            // reuses one cache shard (OpenAI prompt_cache_key / Mistral
            // promptCacheKey / session-affinity headers).
            base_cfg.session_id = Some(effective_session_id.clone());
            if let Some(ref tl) = chosen_thinking {
                base_cfg.thinking_level = Some(tl.clone());
            }
            if let Some(spec) = registry.resolve(&chosen_model) {
                base_cfg.pruning.max_context_tokens = spec.context_window;
                // Prompt-cache warming: enabled only when the model declares
                // both a promptCache lifetime and cost pricing in models.json.
                base_cfg.prompt_cache_warm =
                    spec.prompt_cache_warm_settings(chosen_thinking.as_deref());
            }

            // Transport resolution order: injected factory (in-process tests /
            // embedders) → feature-gated test mock → provider registry (handled
            // inside ThunderRoot when `custom_client` is None).
            let injected = client_factory.as_ref().and_then(|f| f(&base_cfg));

            let root = ThunderRoot::new(base_cfg)
                .with_workspace(ws_dir)
                .with_extra_roots(chosen_shared_roots.iter().map(PathBuf::from).collect())
                .with_provider_registry(registry.clone());
            // Baseline capability set is assembled centrally so every host
            // exposes the same plugins (conversation + skills + mcp + script host).
            let mut root = StandardHostBuilder::new(store.clone())
                .with_script_plugin(script_plugin)
                .build(root);

            // The ask-user capability is opt-in per role: the plugin is only registered
            // when the role enables it, and its auto_always trigger activates it whenever registered.
            let ask_enabled = chosen_role.as_ref().map(|r| r.ask_user).unwrap_or(false);
            if ask_enabled {
                let tool = crate::ask_user::AskUserQuestionTool::new(
                    task_id.clone(),
                    effective_session_id.clone(),
                    output_tx.clone(),
                    pending_questions.clone(),
                );
                root = root.with_plugin(crate::ask_user::AskUserPlugin::new(tool));
            }

            #[cfg(feature = "testing-mock")]
            let mock = if use_mock {
                Some(Arc::new(DaemonMockClient) as Arc<dyn LLMClientTrait>)
            } else {
                None
            };
            #[cfg(not(feature = "testing-mock"))]
            let mock: Option<Arc<dyn LLMClientTrait>> = None;
            let custom_client: Option<Arc<dyn LLMClientTrait>> = injected.or(mock);

            let options = RootRunOptions {
                session_id: Some(effective_session_id.clone()),
                custom_client,
                cancellation_token: Some(cancel_token),
                // Smart baseline set instead of the old blanket forcing:
                // - conversation: one-line prompt cost, keeps session semantics alive
                // - skills: catalog is compact one-liners now; keeps `load_skill` reachable
                // - mcp: only when this workspace actually configures MCP servers
                // Anything else stays keyword-routed and lightweight.
                forced_plugins: Some(baseline_forced_plugins(
                    workspace_has_mcp_config,
                    workspace_has_ts_plugins,
                )),
                register_builtins: true,
                thinking_level: chosen_thinking.clone(),
                role: chosen_role.clone(),
                permission: chosen_permission,
                pause_gate: Some(pause_gate_for_run),
                // Tag every dialog this run raises with its task/session, so a
                // multi-task panel can route the prompt to the right stream.
                ui: Some(Arc::new(
                    host_ui.scoped(run_task_id.clone(), Some(run_session_id)),
                )),
                mode: Some(run_mode),
                // `task_id` is unique per run and readable in panel logs, and it
                // is the key a plugin's reverse RPC is authorised against.
                route: Some(run_task_id),
                policy: Some(run_policy),
            };

            let initial_messages_count = conversation.messages.len();
            let context_input = conversation.as_context_input();
            let start_time_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;

            match root.execute(context_input, options).await {
                Ok(mut handle) => {
                    let selection = handle.selection.clone();
                    let collected_events = Arc::new(tokio::sync::Mutex::new(Vec::new()));
                    let collected_for_stream = Arc::clone(&collected_events);

                    // Stream fine-grained observed events (tokens, tool execution, etc.)
                    if let Some(mut rx) = handle.take_events() {
                        let tid = task_id.clone();
                        let out = output_tx.clone();
                        tokio::spawn(async move {
                            while let Some(event) = rx.recv().await {
                                // Tiered trace retention:
                                // Filter out high-frequency streaming micro-deltas (TokenDelta, ReasoningDelta, ToolCallChunk)
                                // from in-memory trace storage to prevent unbounded memory growth on long tasks, while
                                // still streaming 100% of all events live over stdout for the caller UI.
                                let is_micro_delta = matches!(
                                    event.event,
                                    AgentEvent::TokenDelta { .. }
                                        | AgentEvent::ReasoningDelta { .. }
                                        | AgentEvent::ToolCallChunk { .. }
                                );
                                if !is_micro_delta {
                                    let mut collected = collected_for_stream.lock().await;
                                    if collected.len() < 5000 {
                                        collected.push(event.clone());
                                    }
                                }

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

                            // Load the latest authoritative conversation from store.
                            // The ConversationPlugin records raw, unpruned tool calls and outputs incrementally
                            // during the task run, preserving full history against working-context pruning.
                            let mut conversation = match store.load(&effective_session_id).await {
                                Ok(Some(fresh)) => fresh,
                                _ => conversation,
                            };

                            // Fallback: If ConversationPlugin did not record new messages,
                            // safely append newly generated messages from run_result without overwriting historical turns.
                            if conversation.messages.len() <= initial_messages_count {
                                if res.run_result.messages.len() > initial_messages_count {
                                    conversation.messages.extend(
                                        res.run_result.messages[initial_messages_count..]
                                            .iter()
                                            .cloned(),
                                    );
                                } else if let Some(ref text) = final_content {
                                    if !text.is_empty() {
                                        conversation
                                            .add_assistant_message(Some(text.clone()), None);
                                    }
                                }
                            } else if let Some(ref text) = final_content {
                                // Ensure final assistant content is persisted if not already captured by the plugin
                                let already_present = conversation
                                    .messages
                                    .last()
                                    .map(|m| match m {
                                        ChatMessage::Assistant {
                                            content: Some(c), ..
                                        } => c == text,
                                        _ => false,
                                    })
                                    .unwrap_or(false);
                                if !already_present && !text.is_empty() {
                                    conversation.add_assistant_message(Some(text.clone()), None);
                                }
                            }

                            // Checkpoint compaction fired during this run: the run's
                            // projection (system + checkpoint + kept tail) becomes the
                            // session's authoritative working history so the next task
                            // continues iteratively from the checkpoint, while the full
                            // pre-compaction transcript is preserved alongside for audit.
                            if let Some(raw) = res.run_result.raw_messages.as_ref() {
                                conversation.messages = res.run_result.messages.clone();
                                conversation.recalculate_stats();
                                match store.save_raw_transcript(&effective_session_id, raw).await {
                                    Ok(path) => info!(
                                        session_id = %effective_session_id,
                                        path = %path.display(),
                                        messages = raw.len(),
                                        "persisted raw pre-compaction transcript"
                                    ),
                                    Err(e) => warn!(
                                        session_id = %effective_session_id,
                                        error = %e,
                                        "failed to persist raw transcript"
                                    ),
                                }
                            }

                            let task_tokens = res.run_result.stats.total_prompt_tokens
                                + res.run_result.stats.total_completion_tokens;
                            // `total_tokens` is the working-context estimate (recomputed
                            // from the message list); lifetime usage must be tracked apart
                            // from it or every save silently resets the running total.
                            conversation.stats.total_used_tokens = conversation
                                .stats
                                .total_used_tokens
                                .saturating_add(task_tokens);
                            conversation.stats.turn_count += res.run_result.stats.total_turns;
                            conversation.stats.tool_calls_count +=
                                res.run_result.stats.total_tool_executions;
                            conversation.stats.duration_ms +=
                                res.run_result.stats.total_duration_ms;
                            let _ = store.save(&conversation).await;

                            // Auto-generate concise conversation title on the first turn,
                            // or later whenever the title is still a placeholder (self-heal,
                            // e.g. for conversations created before this feature existed).
                            let is_first_turn =
                                conversation.stats.turn_count <= res.run_result.stats.total_turns;
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
                            let finished_at_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_millis()
                                as u64;
                            // Wall-clock time from task receipt (before root execution,
                            // i.e. right after the user message arrived) to completion.
                            // Distinct from `duration_ms`, which is the agent-loop's
                            // internal duration (used for tokens-per-second math).
                            let wall_duration_ms = finished_at_ms.saturating_sub(start_time_ms);
                            let trace_data = serde_json::json!({
                                "task_id": task_id,
                                "session_id": effective_session_id,
                                "model": chosen_model,
                                "workspace_dir": chosen_workspace,
                                "prompt": prompt,
                                "started_at_ms": start_time_ms,
                                "finished_at_ms": finished_at_ms,
                                "duration_ms": res.run_result.stats.total_duration_ms,
                                "wall_duration_ms": wall_duration_ms,
                                "finish_reason": finish_reason,
                                "stats": res.run_result.stats,
                                "final_content": final_content,
                                "events": events,
                            });
                            save_task_trace(
                                store.root(),
                                &effective_session_id,
                                &task_id,
                                &trace_data,
                            )
                            .await;

                            if res.run_result.finish_reason
                                == thunder_agent_loop::types::event::FinishReason::Error
                            {
                                let err_msg = "Thunder agent task ended with FinishReason::Error (see observed error events for details)".to_string();
                                error!(task_id = %task_id, finish_reason = %finish_reason, "Task finished with error");
                                let msg = DaemonResponse::TaskFailed {
                                    task_id: task_id.clone(),
                                    session_id: Some(effective_session_id.clone()),
                                    error: err_msg,
                                };
                                write_ndjson(&output_tx, &msg).await;
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
                                    final_content: final_content.clone(),
                                    finish_reason: finish_reason.clone(),
                                    active_plugins: selection.active_plugin_ids,
                                };
                                write_ndjson(&output_tx, &msg).await;
                            }
                        }
                        Err(err) => {
                            error!(error = %err, "Task execution failed");
                            let msg = DaemonResponse::TaskFailed {
                                task_id: task_id.clone(),
                                session_id: Some(effective_session_id.clone()),
                                error: err.to_string(),
                            };
                            write_ndjson(&output_tx, &msg).await;
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
                    write_ndjson(&output_tx, &msg).await;
                }
            }

            active_tasks.lock().await.remove(&task_id);
            active_pauses.lock().await.remove(&task_id);
        });
    }
}

async fn write_ndjson(tx: &mpsc::Sender<String>, res: &DaemonResponse) {
    if let Ok(mut json) = serde_json::to_string(res) {
        json.push('\n');
        let _ = tx.send(json).await;
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
///
/// The LLM-side generation now lives in `thunder-agent-providers::naming`
/// (shared with the TUI); this alias keeps the daemon's public surface stable.
pub use thunder_agent_providers::naming::TitleGenError;

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
    // Load the freshest conversation state from the store
    let conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return Err(TitleGenError::new(
                "not_found",
                format!("conversation `{session_id}` not found"),
            ))
        }
        Err(e) => {
            return Err(TitleGenError::new(
                "store_error",
                format!("failed to load conversation: {e}"),
            ))
        }
    };

    // Never overwrite a manual title unless explicitly forced
    if conv.is_title_manual() && !force {
        return Err(TitleGenError::new(
            "manual_locked",
            "title was set manually; pass force=true to override",
        ));
    }

    let prompt = prompt_override
        .or_else(|| first_message_text(&conv, true))
        .ok_or_else(|| {
            TitleGenError::new(
                "empty_title",
                "conversation has no user message to summarize",
            )
        })?;
    let assistant_text = assistant_override.or_else(|| first_message_text(&conv, false));

    let final_title = thunder_agent_providers::naming::generate_title(
        registry,
        &prompt,
        assistant_text.as_deref(),
    )
    .await?;

    // Re-load to avoid clobbering concurrent writes, then persist
    let mut conv = match store.load(session_id).await {
        Ok(Some(c)) => c,
        _ => {
            return Err(TitleGenError::new(
                "store_error",
                format!("conversation `{session_id}` disappeared during title generation"),
            ))
        }
    };
    conv.title = Some(final_title.clone());
    conv.title_source = Some("auto".to_string());
    conv.updated_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    store
        .save(&conv)
        .await
        .map_err(|e| TitleGenError::new("store_error", format!("failed to persist title: {e}")))?;

    info!(session_id = %session_id, title = %final_title, "Conversation title generated");
    Ok(final_title)
}

#[cfg(test)]
mod title_tests {
    use super::*;

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
        conv.messages
            .push(ChatMessage::assistant_text("好的,总结如下"));
        assert_eq!(
            first_message_text(&conv, true).as_deref(),
            Some("帮我总结新闻")
        );
        assert_eq!(
            first_message_text(&conv, false).as_deref(),
            Some("好的,总结如下")
        );
    }
}

#[cfg(test)]
mod ui_protocol_tests {
    //! The `ui_request` / `answer_ui` round trip through the real dispatcher.
    //!
    //! The unit tests in [`crate::ui`] cover the emitter and the timeout; these
    //! cover the half that only exists here: a JSON frame off the wire becoming a
    //! parsed [`DaemonRequest`], and that request actually releasing the caller
    //! that is parked on a dialog.

    use super::*;
    use crate::protocol::DaemonRequest;

    async fn service() -> Arc<DaemonService> {
        Arc::new(
            DaemonService::new(Some(std::env::current_dir().unwrap()))
                .await
                .unwrap(),
        )
    }

    /// Parse a frame the way `main.rs` does, so a serde regression in the
    /// protocol enum fails here rather than in a panel.
    fn parse(raw: serde_json::Value) -> DaemonRequest {
        serde_json::from_str(&raw.to_string()).expect("frame must deserialize")
    }

    /// Wait for the dialog to register itself, then hand back its id.
    async fn await_request_id(svc: &DaemonService) -> String {
        for _ in 0..200 {
            if let Some(id) = svc.pending_ui.lock().await.keys().next().cloned() {
                return id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("no ui_request was registered");
    }

    #[tokio::test]
    async fn answer_ui_releases_a_waiting_dialog() {
        let svc = service().await;

        let asker = {
            let ui = Arc::clone(&svc.host_ui);
            tokio::spawn(async move {
                ui.select_owned(
                    "允许执行 rm -rf build/？",
                    vec!["允许一次".into(), "拒绝".into()],
                )
                .await
            })
        };

        let request_id = await_request_id(&svc).await;
        // The daemon mints the id; a client can only echo it.
        assert!(request_id.starts_with("ui_"), "got {request_id}");

        svc.handle_request(parse(serde_json::json!({
            "method": "answer_ui",
            "id": "req-ui-1",
            "request_id": request_id,
            "value": "拒绝",
        })))
        .await;

        assert_eq!(asker.await.unwrap().as_deref(), Some("拒绝"));
        assert!(
            svc.pending_ui.lock().await.is_empty(),
            "the slot must be released, not leaked"
        );
    }

    #[tokio::test]
    async fn explicit_no_is_routed_as_a_refusal_not_a_cancellation() {
        let svc = service().await;
        let asker = {
            let ui = Arc::clone(&svc.host_ui);
            tokio::spawn(async move { ui.confirm("Proceed?", "writes 3 files").await })
        };
        let request_id = await_request_id(&svc).await;

        svc.handle_request(parse(serde_json::json!({
            "method": "answer_ui",
            "id": "req-ui-2",
            "request_id": request_id,
            "confirmed": false,
        })))
        .await;

        assert!(!asker.await.unwrap());
    }

    #[tokio::test]
    async fn an_answer_for_an_unknown_id_is_inert() {
        let svc = service().await;
        // No dialog is pending, so this must be reported and forgotten rather
        // than parking or panicking.
        svc.handle_request(parse(serde_json::json!({
            "method": "answer_ui",
            "id": "req-ui-3",
            "request_id": "ui_does_not_exist",
            "value": "允许",
        })))
        .await;
        assert!(svc.pending_ui.lock().await.is_empty());
    }

    #[tokio::test]
    async fn dismissal_frame_parses_to_the_cancelled_shape() {
        let req = parse(serde_json::json!({
            "method": "answer_ui",
            "request_id": "ui_x",
            "cancelled": true,
        }));
        match req {
            DaemonRequest::AnswerUi {
                cancelled,
                value,
                confirmed,
                ..
            } => {
                assert!(cancelled);
                assert!(value.is_none());
                assert!(confirmed.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
