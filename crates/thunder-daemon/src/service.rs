use crate::protocol::{
    normalize_mode, DaemonRequest, DaemonResponse, ModeChangeSource, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thunder_core::{
    now_ms, provisional_title, AgentLoopConfig, ContextPruningConfig, Conversation,
    ConversationFilter, ConversationStore, FsConversationStore, LlmDriver, MiddlewarePipeline,
    MiddlewareVerdict, PauseGate, ProviderClientDriver, QueueBehavior, ReActAgentLoop, SteerQueues,
    ThunderConfig, ToolMiddleware,
};
use thunder_executor::{McpServerConfig, McpToolProvider};
use thunder_protocol::{AgentEvent, ChatMessage, EventSink, ObservedEvent, ThunderEvent};
use thunder_providers::prelude::{clamp_title, generate_title, ProviderRegistry, RpiAiClient};
use thunder_registry::{start_hot_reload_watcher, ToolRegistry};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::sync::CancellationToken;

const REQUEST_TIMEOUT_MS: u64 = 120_000;

/// Injectable driver factory for deterministic daemon IPC tests.
pub type DriverFactory = Arc<dyn Fn(&str) -> Option<Arc<dyn LlmDriver>> + Send + Sync>;

#[derive(Clone)]
struct LiveTaskState {
    session_id: String,
    cancel: CancellationToken,
    pause_gate: Arc<PauseGate>,
    steer_queues: Arc<SteerQueues>,
}

struct TaskEventBridge {
    task_id: String,
    agent_id: String,
    output_tx: mpsc::Sender<String>,
}

impl EventSink for TaskEventBridge {
    fn on_event(&self, _event: ThunderEvent) {}

    fn on_agent_event(&self, event: AgentEvent) {
        let frame = DaemonResponse::ObservedEvent {
            task_id: self.task_id.clone(),
            event: ObservedEvent {
                agent_id: self.agent_id.clone(),
                event,
                timestamp_ms: now_ms(),
            },
        };
        if let Ok(line) = serde_json::to_string(&frame) {
            let _ = self.output_tx.try_send(line);
        }
    }
}

struct DaemonPlanModeMiddleware;

#[async_trait::async_trait]
impl ToolMiddleware for DaemonPlanModeMiddleware {
    fn id(&self) -> &str {
        "daemon-plan-mode"
    }

    async fn before_call(
        &self,
        tool_name: &str,
        _arguments: &Value,
        _ctx: &thunder_protocol::ExecutionContext,
    ) -> MiddlewareVerdict {
        let logical = tool_name
            .strip_prefix("core__")
            .or_else(|| tool_name.strip_prefix("script__"))
            .unwrap_or(tool_name);
        if matches!(
            logical,
            "read_file" | "grep" | "find" | "ls" | "thunder__discover_tools"
        ) {
            MiddlewareVerdict::Allow
        } else {
            MiddlewareVerdict::Reject {
                reason: format!("Plan mode refuses non-read-only tool call '{tool_name}'"),
            }
        }
    }
}

pub struct DaemonService {
    provider_registry: Arc<RwLock<ProviderRegistry>>,
    store: Arc<FsConversationStore>,
    active_tasks: Arc<Mutex<HashMap<String, LiveTaskState>>>,
    session_modes: Arc<RwLock<HashMap<String, String>>>,
    output_tx: mpsc::Sender<String>,
    default_workspace: PathBuf,
    driver_factory: Option<DriverFactory>,
    std_bin_override: Option<PathBuf>,
}

impl DaemonService {
    pub async fn new(workspace: Option<PathBuf>) -> Result<(Self, mpsc::Receiver<String>), String> {
        let store = FsConversationStore::new(FsConversationStore::default_store_root())
            .await
            .map_err(|e| format!("Failed to initialize conversation store: {e}"))?;
        Self::with_store(workspace, Arc::new(store)).await
    }

    pub async fn with_store(
        workspace: Option<PathBuf>,
        store: Arc<FsConversationStore>,
    ) -> Result<(Self, mpsc::Receiver<String>), String> {
        let provider_registry = ProviderRegistry::load_default().await.unwrap_or_default();
        let (output_tx, output_rx) = mpsc::channel::<String>(512);
        let default_workspace = workspace
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        Ok((
            Self {
                provider_registry: Arc::new(RwLock::new(provider_registry)),
                store,
                active_tasks: Arc::new(Mutex::new(HashMap::new())),
                session_modes: Arc::new(RwLock::new(HashMap::new())),
                output_tx,
                default_workspace,
                driver_factory: None,
                std_bin_override: None,
            },
            output_rx,
        ))
    }

    pub fn with_driver_factory(mut self, factory: DriverFactory) -> Self {
        self.driver_factory = Some(factory);
        self
    }

    pub fn with_std_binary(mut self, path: PathBuf) -> Self {
        self.std_bin_override = Some(path);
        self
    }

    pub async fn send_response(&self, resp: DaemonResponse) {
        if let Ok(line) = serde_json::to_string(&resp) {
            let _ = self.output_tx.send(line).await;
        }
    }

    pub async fn shutdown(&self) {
        let tasks = self.active_tasks.lock().await;
        for state in tasks.values() {
            state.cancel.cancel();
            state.pause_gate.resume();
        }
    }

    pub async fn handle_request(self: &Arc<Self>, req: DaemonRequest) {
        match req {
            DaemonRequest::Ping { id } => {
                self.reply_ok(
                    id,
                    json!({
                        "status": "pong",
                        "version": env!("CARGO_PKG_VERSION"),
                        "protocol_version": PROTOCOL_VERSION,
                    }),
                )
                .await;
            }
            DaemonRequest::ListModels { id } => {
                let reg = self.provider_registry.read().await;
                let models: Vec<Value> = reg
                    .list_available()
                    .into_iter()
                    .map(|m| {
                        json!({
                            "id": m.id,
                            "name": m.name,
                            "provider": m.provider,
                            "selection_id": m.selection_id(),
                            "context_window": m.context_window,
                            "max_tokens": m.max_tokens,
                            "reasoning": m.reasoning,
                            "available": m.available,
                        })
                    })
                    .collect();
                self.reply_ok(id, json!({ "models": models })).await;
            }
            DaemonRequest::ListConversations { id } => {
                let filter = ConversationFilter::new().with_min_turns(1);
                match self.store.list(&filter).await {
                    Ok(list) => {
                        self.reply_ok(id, json!({ "conversations": list })).await;
                    }
                    Err(e) => self.reply_err(id, e.to_string()).await,
                }
            }
            DaemonRequest::GetConversation { id, session_id } => {
                match self.store.load(&session_id).await {
                    Ok(Some(conv)) => {
                        self.reply_ok(id, serde_json::to_value(conv).unwrap_or_else(|_| json!({})))
                            .await;
                    }
                    Ok(None) => {
                        self.reply_err(id, format!("Conversation '{session_id}' not found"))
                            .await
                    }
                    Err(e) => self.reply_err(id, e.to_string()).await,
                }
            }
            DaemonRequest::SetConversationTitle {
                id,
                session_id,
                title,
            } => {
                let clamped = clamp_title(title);
                if clamped.is_empty() {
                    self.reply_ok(
                        id,
                        json!({
                            "ok": false,
                            "errorKind": "invalid_title",
                            "error": "Title cannot be empty"
                        }),
                    )
                    .await;
                    return;
                }
                match self.store.load(&session_id).await {
                    Ok(Some(mut conv)) => {
                        conv.title = Some(clamped.clone());
                        conv.title_source = Some("manual".to_string());
                        let _ = self.store.save(&conv).await;
                        self.reply_ok(id, json!({ "ok": true, "title": clamped }))
                            .await;
                    }
                    Ok(None) => {
                        self.reply_ok(
                            id,
                            json!({
                                "ok": false,
                                "errorKind": "not_found",
                                "error": format!("Session '{session_id}' not found")
                            }),
                        )
                        .await;
                    }
                    Err(e) => {
                        self.reply_ok(
                            id,
                            json!({
                                "ok": false,
                                "errorKind": "store_error",
                                "error": e.to_string()
                            }),
                        )
                        .await;
                    }
                }
            }
            DaemonRequest::GenerateTitle {
                id,
                session_id,
                force,
            } => match self.store.load(&session_id).await {
                Ok(Some(mut conv)) => {
                    if conv.is_title_manual() && !force {
                        self.reply_ok(
                            id,
                            json!({
                                "ok": false,
                                "errorKind": "manual_locked",
                                "title": conv.title
                            }),
                        )
                        .await;
                        return;
                    }
                    let first_user = conv
                        .messages
                        .iter()
                        .find_map(|m| match m {
                            ChatMessage::User { content, .. } => Some(content.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "New Conversation".to_string());

                    let generated = {
                        let reg = self.provider_registry.read().await;
                        generate_title(&reg, &first_user, conv.model.as_deref())
                            .await
                            .unwrap_or_else(|_| provisional_title(&first_user))
                    };

                    conv.title = Some(generated.clone());
                    conv.title_source = Some("auto".to_string());
                    let _ = self.store.save(&conv).await;
                    self.reply_ok(id, json!({ "ok": true, "title": generated }))
                        .await;
                }
                Ok(None) => {
                    self.reply_ok(
                        id,
                        json!({
                            "ok": false,
                            "errorKind": "not_found",
                            "error": format!("Session '{session_id}' not found")
                        }),
                    )
                    .await;
                }
                Err(e) => {
                    self.reply_ok(
                        id,
                        json!({
                            "ok": false,
                            "errorKind": "store_error",
                            "error": e.to_string()
                        }),
                    )
                    .await;
                }
            },
            DaemonRequest::GetPermissionState { id, session_id } => {
                let mode = self
                    .session_modes
                    .read()
                    .await
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_else(|| "yolo".to_string());
                self.reply_ok(
                    id,
                    json!({
                        "session_id": session_id,
                        "mode": mode,
                        "allowed_rules": []
                    }),
                )
                .await;
            }
            DaemonRequest::SetMode {
                id,
                session_id,
                mode,
            } => {
                let Some(normalized) = normalize_mode(&mode) else {
                    self.reply_err(id, format!("Unsupported mode: '{mode}'"))
                        .await;
                    return;
                };
                let previous = self
                    .session_modes
                    .write()
                    .await
                    .insert(session_id.clone(), normalized.to_string());

                self.reply_ok(
                    id,
                    json!({
                        "session_id": session_id,
                        "mode": normalized,
                        "previous": previous
                    }),
                )
                .await;

                self.send_response(DaemonResponse::ModeChanged {
                    session_id,
                    task_id: None,
                    mode: normalized.to_string(),
                    previous,
                    source: ModeChangeSource::User,
                })
                .await;
            }
            DaemonRequest::PauseTask { id, task_id } => {
                let maybe_state = self.active_tasks.lock().await.get(&task_id).cloned();
                if let Some(state) = maybe_state {
                    state.pause_gate.pause();
                    self.reply_ok(id, json!({ "paused": true, "task_id": task_id }))
                        .await;
                    self.send_response(DaemonResponse::TaskPaused {
                        task_id,
                        session_id: Some(state.session_id),
                        reason: "Paused by user".to_string(),
                    })
                    .await;
                } else {
                    self.reply_err(id, format!("Active task '{task_id}' not found"))
                        .await;
                }
            }
            DaemonRequest::ResumeTask { id, task_id } => {
                let maybe_state = self.active_tasks.lock().await.get(&task_id).cloned();
                if let Some(state) = maybe_state {
                    state.pause_gate.resume();
                    self.reply_ok(id, json!({ "resumed": true, "task_id": task_id }))
                        .await;
                } else {
                    self.reply_err(id, format!("Active task '{task_id}' not found"))
                        .await;
                }
            }
            DaemonRequest::SteerTask {
                id,
                task_id,
                message,
                behavior,
                ..
            } => {
                let maybe_state = self.active_tasks.lock().await.get(&task_id).cloned();
                let Some(state) = maybe_state else {
                    self.reply_err(id, format!("Active task '{task_id}' not found"))
                        .await;
                    return;
                };
                let q_behavior = match behavior.as_deref() {
                    Some("follow_up") => QueueBehavior::FollowUp,
                    _ => QueueBehavior::Steer,
                };
                match q_behavior {
                    QueueBehavior::Steer => {
                        state
                            .steer_queues
                            .steering
                            .enqueue(ChatMessage::user(message));
                    }
                    QueueBehavior::FollowUp => {
                        state
                            .steer_queues
                            .follow_up
                            .enqueue(ChatMessage::user(message));
                    }
                }
                let snap = state.steer_queues.snapshot();
                self.reply_ok(
                    id,
                    json!({
                        "queued": true,
                        "behavior": q_behavior.label(),
                        "steering": snap.steering,
                        "follow_up": snap.follow_up,
                    }),
                )
                .await;
                self.send_response(DaemonResponse::TaskQueueUpdate {
                    task_id,
                    session_id: Some(state.session_id),
                    steering: snap.steering,
                    follow_up: snap.follow_up,
                })
                .await;
            }
            DaemonRequest::ClearQueue { id, task_id } => {
                let maybe_state = self.active_tasks.lock().await.get(&task_id).cloned();
                let Some(state) = maybe_state else {
                    self.reply_err(id, format!("Active task '{task_id}' not found"))
                        .await;
                    return;
                };
                let (s_cleared, f_cleared) = state.steer_queues.clear_all();
                let steering_texts: Vec<String> = s_cleared
                    .iter()
                    .filter_map(|m| m.content_str().map(String::from))
                    .collect();
                let follow_up_texts: Vec<String> = f_cleared
                    .iter()
                    .filter_map(|m| m.content_str().map(String::from))
                    .collect();
                self.reply_ok(
                    id,
                    json!({
                        "steering": steering_texts,
                        "follow_up": follow_up_texts,
                    }),
                )
                .await;
            }
            DaemonRequest::CancelTask { id, task_id } => {
                let maybe_state = self.active_tasks.lock().await.get(&task_id).cloned();
                if let Some(state) = maybe_state {
                    state.cancel.cancel();
                    state.pause_gate.resume();
                    self.reply_ok(id, json!({ "cancelled": true, "task_id": task_id }))
                        .await;
                } else {
                    self.reply_ok(id, json!({ "cancelled": false, "task_id": task_id }))
                        .await;
                }
            }
            DaemonRequest::ReloadIntegrations { id } => {
                if let Ok(fresh) = ProviderRegistry::load_default().await {
                    *self.provider_registry.write().await = fresh;
                }
                self.reply_ok(id, json!({ "reloaded": true })).await;
            }
            DaemonRequest::GetTrace { id, session_id, .. }
            | DaemonRequest::ListTraces { id, session_id } => {
                self.reply_ok(id, json!({ "session_id": session_id, "traces": [] }))
                    .await;
            }
            DaemonRequest::GetMetrics { id, session_id } => {
                let summary = self
                    .store
                    .load(&session_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|c| c.stats)
                    .unwrap_or_default();
                self.reply_ok(
                    id,
                    serde_json::to_value(summary).unwrap_or_else(|_| json!({})),
                )
                .await;
            }
            DaemonRequest::AnswerQuestion { id, .. } | DaemonRequest::AnswerUi { id, .. } => {
                self.reply_ok(id, json!({ "delivered": true })).await;
            }
            DaemonRequest::RunTask {
                id,
                task_id,
                prompt,
                session_id,
                model,
                model_descriptor,
                workspace_dir,
                mode,
                thinking_level,
                ..
            } => {
                let sid = session_id.unwrap_or_else(|| format!("sess-{}", now_ms()));
                let ws_path = workspace_dir
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.default_workspace.clone());

                if let Some(ref m) = mode {
                    if let Some(normalized) = normalize_mode(m) {
                        let prev = self
                            .session_modes
                            .write()
                            .await
                            .insert(sid.clone(), normalized.to_string());
                        self.send_response(DaemonResponse::ModeChanged {
                            session_id: sid.clone(),
                            task_id: Some(task_id.clone()),
                            mode: normalized.to_string(),
                            previous: prev,
                            source: ModeChangeSource::RunOption,
                        })
                        .await;
                    }
                }

                let active_mode = self
                    .session_modes
                    .read()
                    .await
                    .get(&sid)
                    .cloned()
                    .unwrap_or_else(|| "yolo".to_string());

                // Resolve LLM driver
                let resolved_driver = match self
                    .resolve_driver(
                        model.as_deref(),
                        model_descriptor.as_deref(),
                        thinking_level.as_deref(),
                        &sid,
                    )
                    .await
                {
                    Ok(d) => d,
                    Err(err) => {
                        self.reply_err(id, err.clone()).await;
                        self.send_response(DaemonResponse::TaskFailed {
                            task_id,
                            session_id: Some(sid),
                            error: err,
                            finish_reason: None,
                        })
                        .await;
                        return;
                    }
                };

                self.reply_ok(
                    id,
                    json!({
                        "started": true,
                        "task_id": task_id,
                        "session_id": sid,
                    }),
                )
                .await;

                let cancel = CancellationToken::new();
                let pause_gate = PauseGate::new_shared();
                let steer_queues = SteerQueues::new_shared();

                self.active_tasks.lock().await.insert(
                    task_id.clone(),
                    LiveTaskState {
                        session_id: sid.clone(),
                        cancel: cancel.clone(),
                        pause_gate: Arc::clone(&pause_gate),
                        steer_queues: Arc::clone(&steer_queues),
                    },
                );

                let svc = Arc::clone(self);
                tokio::spawn(async move {
                    svc.execute_task(
                        task_id,
                        sid,
                        prompt,
                        ws_path,
                        active_mode,
                        resolved_driver,
                        cancel,
                        pause_gate,
                        steer_queues,
                    )
                    .await;
                });
            }
        }
    }

    async fn resolve_driver(
        &self,
        model_name: Option<&str>,
        descriptor: Option<&thunder_providers::ModelDescriptor>,
        thinking_level: Option<&str>,
        session_id: &str,
    ) -> Result<(String, Arc<dyn LlmDriver>), String> {
        if let Some(ref factory) = self.driver_factory {
            let label = model_name.unwrap_or("mock-model").to_string();
            if let Some(driver) = factory(&label) {
                return Ok((label, driver));
            }
        }

        if let Some(desc) = descriptor {
            let (client, _) = RpiAiClient::from_descriptor(desc, REQUEST_TIMEOUT_MS)?;
            let label = format!("{}/{}", desc.provider, desc.id);
            let mut drv = ProviderClientDriver::new(Arc::new(client))
                .with_model(desc.id.clone())
                .with_session_id(session_id);
            if let Some(lvl) = thinking_level {
                drv = drv.with_thinking_level(lvl);
            }
            return Ok((label, Arc::new(drv)));
        }

        let reg = self.provider_registry.read().await;
        let spec = if let Some(sel) = model_name {
            reg.resolve(sel)
                .cloned()
                .ok_or_else(|| format!("Model '{sel}' not found in provider registry"))?
        } else {
            reg.list_available()
                .into_iter()
                .find(|m| m.available)
                .cloned()
                .ok_or_else(|| "No available LLM model in provider registry".to_string())?
        };

        let client =
            thunder_providers::client_for(&spec, REQUEST_TIMEOUT_MS).map_err(|e| e.to_string())?;
        let mut drv = ProviderClientDriver::new(client)
            .with_model(spec.id.clone())
            .with_session_id(session_id);
        if let Some(lvl) = thinking_level {
            drv = drv.with_thinking_level(lvl);
        }
        Ok((spec.selection_id(), Arc::new(drv)))
    }

    async fn execute_task(
        &self,
        task_id: String,
        session_id: String,
        prompt: String,
        workspace_cwd: PathBuf,
        mode: String,
        (model_label, driver): (String, Arc<dyn LlmDriver>),
        cancel: CancellationToken,
        pause_gate: Arc<PauseGate>,
        steer_queues: Arc<SteerQueues>,
    ) {
        let cfg = ThunderConfig::load_from_workspace(&workspace_cwd).await;
        let registry = self.build_workspace_registry(&workspace_cwd, &cfg).await;
        let tools_dir = workspace_cwd.join(".thunder").join("tools");
        let _watcher = start_hot_reload_watcher(registry.clone(), tools_dir, None)
            .await
            .ok();

        let existing_conv = self.store.load(&session_id).await.ok().flatten();

        let mut conv = existing_conv.unwrap_or_else(|| {
            Conversation::new(session_id.clone())
                .with_model(model_label.clone())
                .with_workspace(workspace_cwd.to_string_lossy().into_owned())
        });
        if conv.title.is_none() {
            conv.title = Some(provisional_title(&prompt));
        }

        let loop_cfg = AgentLoopConfig {
            session_id: session_id.clone(),
            workspace_cwd: workspace_cwd.clone(),
            system_prompt: "You are Thunder Agent, a microkernel coding assistant.".to_string(),
            model: model_label,
            max_turns: cfg.agent.max_turns,
            pruning: ContextPruningConfig {
                max_context_tokens: cfg.agent.max_context_tokens,
                ..ContextPruningConfig::default()
            },
            tool_timeout: Duration::from_millis(cfg.sandbox.timeout_ms),
            cache_warm_settings: None,
        };

        let mut mw = MiddlewarePipeline::new();
        if mode == "plan" {
            mw = mw.with_middleware(Arc::new(DaemonPlanModeMiddleware));
        }

        let bridge = Arc::new(TaskEventBridge {
            task_id: task_id.clone(),
            agent_id: format!("agent-{session_id}"),
            output_tx: self.output_tx.clone(),
        });

        let mut agent = ReActAgentLoop::new(loop_cfg, registry, driver)
            .with_initial_messages(conv.messages.clone())
            .with_cancellation_token(cancel)
            .with_pause_gate(pause_gate)
            .with_steer_queues(steer_queues)
            .with_middleware(mw)
            .with_event_sink(bridge);

        let run_res = agent.run(prompt).await;
        self.active_tasks.lock().await.remove(&task_id);

        conv.set_messages(agent.context().get_messages());
        let _ = self.store.save(&conv).await;

        match run_res {
            Ok(outcome) => {
                self.send_response(DaemonResponse::TaskCompleted {
                    task_id,
                    session_id,
                    final_content: Some(outcome.final_response),
                    finish_reason: "done".to_string(),
                    active_plugins: vec!["thunder-std".to_string(), "kv-cache-shield".to_string()],
                })
                .await;
            }
            Err(err) => {
                let reason = if err.contains("cancelled") {
                    Some("Cancelled".to_string())
                } else {
                    Some("Error".to_string())
                };
                self.send_response(DaemonResponse::TaskFailed {
                    task_id,
                    session_id: Some(session_id),
                    error: err,
                    finish_reason: reason,
                })
                .await;
            }
        }
    }

    async fn build_workspace_registry(
        &self,
        workspace_cwd: &Path,
        config: &ThunderConfig,
    ) -> ToolRegistry {
        let registry = ToolRegistry::new();
        if let Some(std_bin) = self
            .std_bin_override
            .clone()
            .or_else(locate_thunder_std_binary)
        {
            let std_cfg = McpServerConfig::new(std_bin).as_core();
            if let Ok(provider) = McpToolProvider::connect("thunder-std", &std_cfg).await {
                let _ = registry.register_provider(provider).await;
            }
        }
        for (name, mcp_cfg) in &config.mcp_servers {
            if let Ok(provider) = McpToolProvider::connect(name, mcp_cfg).await {
                let _ = registry.register_provider(provider).await;
            }
        }
        let tools_dir = workspace_cwd.join(".thunder").join("tools");
        if tools_dir.exists() {
            let _ = registry.reload_scripts_dir(&tools_dir).await;
        }
        registry
    }

    async fn reply_ok(&self, id: Option<String>, data: Value) {
        self.send_response(DaemonResponse::Response {
            id,
            success: true,
            data: Some(data),
            error: None,
        })
        .await;
    }

    async fn reply_err(&self, id: Option<String>, error: impl Into<String>) {
        self.send_response(DaemonResponse::Response {
            id,
            success: false,
            data: None,
            error: Some(error.into()),
        })
        .await;
    }
}

fn locate_thunder_std_binary() -> Option<PathBuf> {
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("thunder-std");
            if sibling.exists() {
                return Some(sibling);
            }
            if let Some(parent) = dir.parent() {
                let parent_sibling = parent.join("thunder-std");
                if parent_sibling.exists() {
                    return Some(parent_sibling);
                }
            }
        }
    }
    None
}
