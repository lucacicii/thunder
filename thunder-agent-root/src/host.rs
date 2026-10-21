use crate::error::PluginError;
use crate::instructions::discover_context_files;
use crate::plugin::PluginContext;
use crate::plugins::metrics::{MetricsPlugin, MetricsSnapshot, METRICS_UPDATE_EVENT_KIND};
use crate::plugins::mode::{Mode, ModePlugin};
use crate::plugins::status::{StatusPlugin, AGENT_STATUS_EVENT_KIND};
use crate::registry::{ActivePluginSet, PluginRegistry};
use crate::selector::{PluginSelection, PluginSelector};
use crate::thunder_config::ThunderConfig;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::registry_tools::StaticToolsFactory;
use thunder_agent_policy::gate::ToolGate;
use thunder_agent_types::scope::ToolScope;
use thunder_agent_loop::loop_engine::handle::AgentHandle;
use thunder_agent_loop::stream::client::LLMClientTrait;
use thunder_agent_policy::invoke::{
    empty_tool_invoker_slot, ExecutorToolInvoker, ToolInvokerSlot,
};
use thunder_agent_policy::ui::HostUi;
use thunder_agent_loop::{
    AgentConfig, AgentError, AgentLoop, AgentRunResult, ChatMessage, ContextInput, ObservedEvent,
};
use thunder_agent_pack_code::{autonomous_system_prompt, RenderCapabilities};
use thunder_agent_providers::prelude::{client_for, ModelRef, ProviderRegistry};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct RootRunOptions {
    pub session_id: Option<String>,
    pub custom_client: Option<Arc<dyn LLMClientTrait>>,
    pub cancellation_token: Option<CancellationToken>,
    pub forced_plugins: Option<Vec<String>>,
    pub register_builtins: bool,
    pub thinking_level: Option<String>,
    /// The mode this run is judged in. Hosts that did not choose default to
    /// `yolo`, which is what "no gate installed" has always meant.
    pub mode: Mode,
    /// Optional cooperative pause gate forwarded to the agent unit, letting the
    /// host freeze the run at a tool boundary and resume it later.
    pub pause_gate: Option<Arc<thunder_agent_loop::core::pause::PauseGate>>,
    /// Optional steering / follow-up queues forwarded to the agent unit, letting
    /// the host splice user input into a run already in flight.
    pub steer_queues: Option<Arc<thunder_agent_loop::core::steer::SteerQueues>>,
    /// Per-run override of the host UI. Hosts pass a task-scoped handle so the
    /// panel can attribute a dialog to the run that raised it; `None` falls back
    /// to the root's [`ThunderRoot::with_host_ui`] surface.
    pub ui: Option<Arc<dyn HostUi>>,
    /// Explicit run identifier. Defaults to a generated one.
    ///
    /// Hosts pass their own (the daemon uses `task_id`) so the id is meaningful
    /// in logs and panel routing. It must be unique among *concurrent* runs: it
    /// is the key that decides which run's authority a plugin call spends.
    pub route: Option<String>,
    /// Session-scoped mode state. Hosts pass the *same* plugin across every
    /// run of a session so "always allow" rules and mid-session mode switches
    /// survive a turn; `None` creates a fresh one for this run only.
    pub mode_plugin: Option<Arc<ModePlugin>>,
    /// What the surface this run talks to can draw. The default prompt's
    /// presentation rules are assembled from it, so a host that cannot render
    /// diagrams never asks the model for one.
    pub render_capabilities: RenderCapabilities,
}

impl Default for RootRunOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            custom_client: None,
            cancellation_token: None,
            forced_plugins: None,
            register_builtins: true,
            thinking_level: None,
            mode: Mode::default(),
            mode_plugin: None,
            pause_gate: None,
            steer_queues: None,
            ui: None,
            route: None,
            render_capabilities: RenderCapabilities::default(),
        }
    }
}

pub struct RootRunResult {
    pub agent_id: String,
    pub final_content: Option<String>,
    pub selection: PluginSelection,
    pub run_result: AgentRunResult,
    pub metrics: Option<MetricsSnapshot>,
}

pub struct RootRunHandle {
    pub agent_id: String,
    pub selection: PluginSelection,
    pub handle: AgentHandle,
    pub active_set: ActivePluginSet,
    pub ctx: PluginContext,
    /// Signalled when the event proxy has drained the loop's stream and
    /// finished dispatching every event to the active plugins.
    ///
    /// [`Self::join`] awaits it: a plugin's `on_event` may write (the
    /// conversation plugin checkpoints the session on `TurnEnd`), and returning
    /// from `join` before that write lands would let a host's own read-modify-
    /// write of the same store race it.
    dispatch_drained: Option<tokio::sync::oneshot::Receiver<()>>,
}

/// Emit the run's execution status when it moved since the last frame.
///
/// Deduplicated by `AgentStatusSnapshot::seq`, unlike the metrics snapshot
/// beside it: metrics ride along with every relevant event, whereas a status is
/// a *transition*, and re-publishing an unchanged one would make every token
/// look like a state change to a consumer.
///
/// Returns `false` when the consumer is gone, which ends the proxy.
async fn publish_status(
    tx: &mpsc::Sender<ObservedEvent>,
    status_plugin: Option<&StatusPlugin>,
    session_id: &str,
    agent_id: &str,
    published: &mut Option<u64>,
) -> bool {
    let Some(plugin) = status_plugin else {
        return true;
    };
    let Some(snapshot) = plugin.snapshot(session_id) else {
        return true;
    };
    if *published == Some(snapshot.seq) {
        return true;
    }
    let Ok(payload) = serde_json::to_value(&snapshot) else {
        return true;
    };
    *published = Some(snapshot.seq);
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);

    tx.send(ObservedEvent {
        agent_id: agent_id.to_string(),
        event: thunder_agent_loop::types::event::AgentEvent::Custom {
            kind: AGENT_STATUS_EVENT_KIND.to_string(),
            payload,
        },
        timestamp_ms,
    })
    .await
    .is_ok()
}

impl RootRunHandle {
    /// Take the observed event stream.
    ///
    /// Events flow through a non-blocking proxy that dispatches
    /// [`ActivePluginSet::dispatch_event`] to all active plugins (including
    /// [`MetricsPlugin`] and [`ConversationPlugin`]) and emits live
    /// `AgentEvent::Custom { kind: "metrics_update" }` and
    /// `AgentEvent::Custom { kind: "agent_status" }` snapshots for downstream
    /// hosts (TUI, daemon, desktop panel).
    ///
    /// The status frame is published whenever the plugin's `seq` moves, and the
    /// proxy also wakes on the plugin's own change signal — a host-reported wait
    /// (`awaiting_user`) has no kernel event to ride on, and during one there is
    /// no further event at all: the gate is blocked inside tool execution. See
    /// [`StatusPlugin`].
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<ObservedEvent>> {
        let mut inner_rx = self.handle.take_events()?;
        let (tx, rx) = mpsc::channel(512);
        let (drained_tx, drained_rx) = tokio::sync::oneshot::channel();
        self.dispatch_drained = Some(drained_rx);
        let active_set = self.active_set.clone();
        let ctx = self.ctx.clone();
        let metrics_plugin = self.ctx.metrics_plugin().cloned();
        let status_plugin = self.ctx.status_plugin().cloned();
        let mut status_changes = status_plugin.as_ref().map(StatusPlugin::subscribe_changes);
        let session_id = self.ctx.session_id.clone();
        let run_agent_id = self.agent_id.clone();

        tokio::spawn(async move {
            let mut published_status: Option<u64> = None;

            loop {
                let observed = match status_changes.as_mut() {
                    Some(changes) => {
                        tokio::select! {
                            event = inner_rx.recv() => event,
                            changed = changes.changed() => {
                                if changed.is_err()
                                    || !publish_status(
                                        &tx,
                                        status_plugin.as_ref(),
                                        &session_id,
                                        &run_agent_id,
                                        &mut published_status,
                                    )
                                    .await
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                    }
                    None => inner_rx.recv().await,
                };
                let Some(observed) = observed else {
                    break;
                };

                active_set.dispatch_event(&observed, &ctx).await;
                let snap = metrics_plugin
                    .as_ref()
                    .and_then(|mp| mp.snapshot(&session_id));
                let agent_id = observed.agent_id.clone();
                let ts = observed.timestamp_ms;
                let is_relevant = matches!(
                    &observed.event,
                    thunder_agent_loop::types::event::AgentEvent::TurnStart { .. }
                        | thunder_agent_loop::types::event::AgentEvent::TokenDelta { .. }
                        | thunder_agent_loop::types::event::AgentEvent::ReasoningDelta { .. }
                        | thunder_agent_loop::types::event::AgentEvent::ToolExecStart { .. }
                        | thunder_agent_loop::types::event::AgentEvent::ToolExecResult { .. }
                        | thunder_agent_loop::types::event::AgentEvent::ContextCompacted { .. }
                        | thunder_agent_loop::types::event::AgentEvent::TurnEnd { .. }
                        | thunder_agent_loop::types::event::AgentEvent::LoopComplete { .. }
                );
                let is_micro = matches!(
                    &observed.event,
                    thunder_agent_loop::types::event::AgentEvent::TokenDelta { .. }
                        | thunder_agent_loop::types::event::AgentEvent::ReasoningDelta { .. }
                );

                if tx.send(observed).await.is_err() {
                    break;
                }

                if is_relevant {
                    if let Some(snapshot) = snap {
                        if let Ok(payload) = serde_json::to_value(&snapshot) {
                            let custom = ObservedEvent {
                                agent_id,
                                event: thunder_agent_loop::types::event::AgentEvent::Custom {
                                    kind: METRICS_UPDATE_EVENT_KIND.to_string(),
                                    payload,
                                },
                                timestamp_ms: ts,
                            };
                            if is_micro {
                                let _ = tx.try_send(custom);
                            } else if tx.send(custom).await.is_err() {
                                break;
                            }
                        }
                    }
                }

                if !publish_status(
                    &tx,
                    status_plugin.as_ref(),
                    &session_id,
                    &run_agent_id,
                    &mut published_status,
                )
                .await
                {
                    break;
                }
            }
            // The loop's stream is closed and every event has been dispatched:
            // release `join`, which is waiting on exactly this.
            let _ = drained_tx.send(());
        });

        Some(rx)
    }

    pub fn metrics_snapshot(&self) -> Option<MetricsSnapshot> {
        self.ctx
            .metrics_plugin()
            .and_then(|mp| mp.snapshot(&self.ctx.session_id))
    }

    pub fn status_snapshot(&self) -> Option<crate::plugins::AgentStatusSnapshot> {
        self.ctx
            .status_plugin()
            .and_then(|sp| sp.snapshot(&self.ctx.session_id))
    }

    pub async fn join(mut self) -> Result<RootRunResult, AgentError> {
        let run_result = self.handle.join().await?;
        // The loop is done, but its event proxy may still be dispatching the
        // tail of the stream to plugins. Wait for that before `on_finish`, so
        // both halves of the same run settle in order.
        if let Some(drained) = self.dispatch_drained.take() {
            let _ = drained.await;
        }
        if let Err(err) = self
            .active_set
            .dispatch_finish(&run_result, &self.ctx)
            .await
        {
            warn!(
                session_id = %self.ctx.session_id,
                error = %err,
                "Plugin on_finish lifecycle hook returned an error"
            );
        }
        let metrics = self
            .ctx
            .metrics_plugin()
            .and_then(|mp| mp.snapshot(&self.ctx.session_id));

        Ok(RootRunResult {
            agent_id: self.agent_id,
            final_content: run_result.final_content.clone(),
            selection: self.selection,
            run_result,
            metrics,
        })
    }
}

/// Source of per-run route ids, so two runs of one session never collide.
static RUN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
/// Distinguishes auto-named sessions created in the same millisecond.
static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct ThunderRoot {
    config: AgentConfig,
    registry: PluginRegistry,
    selector: PluginSelector,
    workspace_root: Option<PathBuf>,
    /// Extra roots (e.g. referenced repositories) sharing the workspace's
    /// read/write standing, alongside the primary workspace.
    extra_roots: Vec<PathBuf>,
    scratch_root: PathBuf,
    provider_registry: ProviderRegistry,
    active_model: ModelRef,
    /// Default host UI handed to runs that do not override it per-run.
    ///
    /// Defaults to `NullHostUi`: no panel, every dialog cancelled, every caller
    /// degrades to its safe branch.
    host_ui: Arc<dyn HostUi>,
    metrics_plugin: Option<MetricsPlugin>,
    status_plugin: Option<StatusPlugin>,
    initial_session_total_tokens: Option<usize>,
}
impl ThunderRoot {
    pub fn new(config: AgentConfig) -> Self {
        let scratch_root = std::env::temp_dir().join("thunder_root_scratch");
        // Best-effort cleanup of stale per-session scratch dirs. Only schedule
        // it when a Tokio runtime is present: `ThunderRoot::new` is also called
        // from synchronous constructors (tests), and spawning there would
        // panic. Skipping it in that case is harmless — cleanup is opportunistic.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let scratch_clean = scratch_root.clone();
            handle.spawn(async move {
                clean_stale_scratch_dirs(&scratch_clean, std::time::Duration::from_secs(7 * 86400))
                    .await;
            });
        }

        let selector = PluginSelector::new(Some(config.clone()));
        let active_model = ModelRef::parse(&config.model);
        let extra_roots = config.extra_workspace_roots.clone();
        Self {
            config,
            registry: PluginRegistry::new(),
            selector,
            workspace_root: None,
            extra_roots,
            scratch_root,
            provider_registry: ProviderRegistry::default(),
            active_model,
            host_ui: Arc::new(thunder_agent_policy::ui::NullHostUi),
            metrics_plugin: None,
            status_plugin: None,
            initial_session_total_tokens: None,
        }
    }

    /// Provide the host's user-interaction surface (dialogs, notifications).
    ///
    /// A run may override it with [`RootRunOptions::ui`] — typically to attach a
    /// task id so a panel can route the dialog to the right run.
    pub fn with_host_ui(mut self, ui: Arc<dyn HostUi>) -> Self {
        self.host_ui = ui;
        self
    }

    pub async fn with_providers(mut self) -> Self {
        if let Ok(registry) = ProviderRegistry::load_from_sources(
            &thunder_agent_providers::source::ConfigSource::default_chain(
                self.workspace_root.as_deref(),
            ),
        )
        .await
        {
            self.provider_registry = registry;
        }
        self
    }

    pub fn with_provider_registry(mut self, registry: ProviderRegistry) -> Self {
        self.provider_registry = registry;
        self
    }

    pub fn set_model(&mut self, model: ModelRef) {
        self.active_model = model.clone();
        self.config.model = model.selection_id();
    }

    pub fn active_model(&self) -> &ModelRef {
        &self.active_model
    }

    pub fn provider_registry(&self) -> &ProviderRegistry {
        &self.provider_registry
    }

    /// Attach a shared [`MetricsPlugin`] so session telemetry (token rates,
    /// cache hit rates, context occupancy, durations) persists across runs.
    pub fn with_metrics_plugin(mut self, plugin: MetricsPlugin) -> Self {
        self.metrics_plugin = Some(plugin.clone());
        self.registry.register(plugin);
        self
    }

    /// Seed the cumulative billed tokens for the session prior to this run.
    pub fn with_initial_session_total_tokens(mut self, total: usize) -> Self {
        self.initial_session_total_tokens = Some(total);
        self
    }

    /// Attach the execution-status plugin, so every run publishes what the agent
    /// is doing right now (phase, current tool, turn).
    ///
    /// The caller keeps a clone to report phases the event stream cannot express
    /// — see [`StatusPlugin::note_awaiting`]. Passing one in rather than letting
    /// the baseline set allocate its own is what lets a host *hold* the status it
    /// publishes.
    pub fn with_status_plugin(mut self, plugin: StatusPlugin) -> Self {
        self.status_plugin = Some(plugin.clone());
        self.registry.register(plugin);
        self
    }

    pub fn with_plugin<P: crate::plugin::ThunderPlugin + 'static>(mut self, plugin: P) -> Self {
        self.registry.register(plugin);
        self
    }

    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.config.workspace_dir = Some(path.clone());
        self.workspace_root = Some(path);
        self
    }

    /// Grant extra roots (e.g. repositories referenced by the task) the same
    /// read/write standing as the primary workspace. Relative paths keep
    /// resolving against the primary workspace; these roots accept absolute
    /// paths for reads, writes, and shell targets.
    pub fn with_extra_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.config.extra_workspace_roots = roots.clone();
        self.extra_roots = roots;
        self
    }

    pub fn with_scratch_root(mut self, path: PathBuf) -> Self {
        self.scratch_root = path;
        self
    }

    /// Inject an isolated session-selection cache.
    ///
    /// Roots otherwise share the process-wide default, which is what keeps a
    /// session's plugin set stable across the per-request rebuilds that both the
    /// TUI and the daemon perform. Pass a fresh [`SelectionCache`] when two
    /// hosts are embedded in one process and must not share selection state.
    pub fn with_selection_cache(
        mut self,
        cache: std::sync::Arc<crate::selector::SelectionCache>,
    ) -> Self {
        self.selector = self.selector.with_cache(cache);
        self
    }

    pub fn registry(&self) -> &PluginRegistry {
        &self.registry
    }

    pub fn registry_mut(&mut self) -> &mut PluginRegistry {
        &mut self.registry
    }

    pub fn selector(&self) -> &PluginSelector {
        &self.selector
    }

    /// Drop the cached per-session plugin selection so the next execute()
    /// re-runs the heuristic (e.g. after plugin reloads or registry changes).
    ///
    /// Uses *this* root's cache, so it stays correct even when an isolated
    /// cache was injected.
    pub fn invalidate_session_selection(&self, session_id: &str) {
        self.selector.cache().invalidate(session_id);
    }

    /// Drop the cached selection for **every** session on this root's cache.
    ///
    /// A registry-wide change (plugin reload, plugin set change) can invalidate
    /// any session's cached set, so reload paths should call this rather than
    /// guessing which session ids are live.
    pub fn invalidate_all_session_selections(&self) {
        self.selector.cache().invalidate_all();
    }

    /// Autonomously select plugins, initialize context, configure and start the root AgentLoop unit.
    pub async fn execute(
        &self,
        input: impl Into<ContextInput>,
        options: RootRunOptions,
    ) -> Result<RootRunHandle, PluginError> {
        let context_input: ContextInput = input.into();
        let prompt_str = match &context_input {
            ContextInput::Text(t) => t.clone(),
            ContextInput::Messages(msgs) => msgs
                .iter()
                .rev()
                .find_map(|m| match m {
                    ChatMessage::User { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "General task".to_string()),
            ContextInput::Buffer(buf) => buf
                .get_messages()
                .iter()
                .rev()
                .find_map(|m| match m {
                    ChatMessage::User { content, .. } => Some(content.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "General task".to_string()),
        };

        // Project/user agent configuration (`.thunder/config.json`). Merged here
        // so the appended prompt file and the turn budget reflect the workspace
        // this run is bound to.
        let thunder_cfg = ThunderConfig::load(self.workspace_root.as_deref()).await;

        // A run that names no session gets one of its own. The previous
        // fallback was a constant (`sess_1`), which made every unnamed run
        // share one conversation store entry, one mode/rule set and one
        // status/metrics slot — two such runs in the same process silently
        // overwrote each other's history.
        let session_id = options.session_id.unwrap_or_else(|| {
            let seq = SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!(
                "sess_{}_{seq}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            )
        });

        let cancel = options.cancellation_token.unwrap_or_default();

        // A stable, unique id for this *run* (not this session — several tasks
        // can share one). Everything a plugin can reach is authorised against
        // the run it belongs to, and the sidecar is shared by all of them.
        let route = options.route.clone().unwrap_or_else(|| {
            let seq = RUN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            format!("run_{}_{}", session_id, seq)
        });

        // 1. Select active plugins — session-locked: the first message decides
        // for the whole session so the combined prompt + toolset stay stable.
        let selection = if let Some(forced) = options.forced_plugins {
            PluginSelection {
                active_plugin_ids: forced,
                reason: "Explicitly forced by execution options".to_string(),
                confidence: 1.0,
            }
        } else {
            self.selector
                .select_for_session(Some(&session_id), &prompt_str, &self.registry)
                .await
        };

        info!(
            session_id = %session_id,
            active_plugins = ?selection.active_plugin_ids,
            reason = %selection.reason,
            "ThunderRoot executing prompt"
        );

        let mut active_set = self
            .registry
            .create_active_set(&selection.active_plugin_ids);

        // 1b. Resolve the mode this run is judged in.
        //
        // A host-supplied plugin is reused across the session (so remembered
        // rules and mid-session mode switches survive); a fresh one runs in the
        // mode the options named. The gate always installs — the mode is what
        // decides whether it ever asks.
        let mode_plugin = options
            .mode_plugin
            .clone()
            .unwrap_or_else(|| ModePlugin::new(options.mode));
        mode_plugin.set_mode(options.mode);
        // Bind the plan file's location and name. The plugin is shared across a
        // session's runs, so the binding is refreshed per run rather than only
        // at construction — otherwise a session that changed workspace would
        // still plan into the first one's directory.
        mode_plugin.bind_session(self.workspace_root.clone(), &session_id);
        let ui = options
            .ui
            .clone()
            .unwrap_or_else(|| Arc::clone(&self.host_ui));

        // The mode plugin is the host's own, not a registry selection, so it has
        // to be added to the active set explicitly: the gate it provides judges
        // every call, and the plan tools it carries are how the model leaves
        // plan mode at all.
        active_set.push(Arc::clone(&mode_plugin) as Arc<dyn crate::plugin::ThunderPlugin>);

        // Install the telemetry / token-speed plugin so every run tracks live
        // tok/s, context occupancy, prompt-cache hit rates, and wall-clock time.
        let metrics_plugin = self.metrics_plugin.clone().unwrap_or_default();
        let initial_ctx_tokens = match &context_input {
            ContextInput::Text(t) => {
                thunder_agent_loop::core::token_estimator::estimate_token_count(t)
            }
            ContextInput::Messages(msgs) => msgs
                .iter()
                .filter_map(|m| m.content_str())
                .map(thunder_agent_loop::core::token_estimator::estimate_token_count)
                .sum(),
            ContextInput::Buffer(buf) => buf.estimated_tokens(),
        };
        let resolved_spec = self.provider_registry.resolve_ref(&self.active_model);
        let context_window_limit = resolved_spec
            .map(|s| s.context_window)
            .filter(|&w| w > 0)
            .or_else(|| {
                let w = self.config.pruning.max_context_tokens;
                (w > 0).then_some(w)
            });
        let effective_thinking = options
            .thinking_level
            .clone()
            .or_else(|| self.config.thinking_level.clone());
        metrics_plugin.configure_session(
            &session_id,
            Some(self.active_model.selection_id()),
            effective_thinking,
            context_window_limit,
            initial_ctx_tokens,
            self.initial_session_total_tokens.unwrap_or(0),
        );
        if !selection.active_plugin_ids.iter().any(|id| id == "metrics") {
            active_set
                .push(Arc::new(metrics_plugin.clone()) as Arc<dyn crate::plugin::ThunderPlugin>);
        }

        // Same treatment for the execution status: a host that asked for it gets
        // it on every run, whether or not the keyword selector picked it.
        let status_plugin = self.status_plugin.clone().unwrap_or_default();
        if !selection
            .active_plugin_ids
            .iter()
            .any(|id| id == StatusPlugin::ID)
        {
            active_set
                .push(Arc::new(status_plugin.clone()) as Arc<dyn crate::plugin::ThunderPlugin>);
        }

        info!(
            session_id = %session_id,
            mode = options.mode.as_str(),
            "Resolved mode"
        );

        // 2. Prepare Plugin Context
        // The mode is carried here so a plugin that frames guidance to the
        // model can speak of the mode the user chose.
        // Filled in after the agent exists — see the note at the fill site.
        let tool_slot: ToolInvokerSlot = empty_tool_invoker_slot();
        let mut ctx = PluginContext::new(&session_id)
            .with_cancellation(cancel.clone())
            .with_mode(options.mode)
            .with_ui(Arc::clone(&ui))
            .with_route(route.clone())
            .with_mode_plugin(Arc::clone(&mode_plugin))
            .with_metrics_plugin(metrics_plugin)
            .with_status_plugin(status_plugin)
            .with_prompt(prompt_str.clone())
            .with_tool_slot(Arc::clone(&tool_slot));
        if let Some(ws) = &self.workspace_root {
            ctx = ctx.with_workspace(ws.clone());
        }
        ctx = ctx.with_scratch(self.scratch_root.join(&session_id));

        // 3. Dispatch plugin on_init lifecycle
        active_set.dispatch_init(&ctx).await?;

        // 4. Construct Root AgentLoop
        let default_prompt = autonomous_system_prompt(options.render_capabilities);
        let mut base_prompt = self
            .config
            .system_prompt
            .as_deref()
            .unwrap_or(default_prompt.as_str())
            .to_string();
        // 4a. Discover and append AGENTS.md / CLAUDE.md instruction context files (aligning with Pi)
        // Two roots: the user's own cross-tool dirs, and thunder's data root, which
        // an embedding host may have pointed somewhere private.
        let real_home = thunder_agent_loop::core::paths::real_home_dir();
        let thunder_root = thunder_agent_loop::core::paths::thunder_config_dir();
        let context_files = discover_context_files(
            self.workspace_root.as_deref(),
            real_home.as_deref(),
            Some(thunder_root.as_path()),
        )
        .await;
        for ctx_file in context_files {
            base_prompt.push_str("\n\n### Context Instructions (");
            base_prompt.push_str(&ctx_file.path.display().to_string());
            base_prompt.push_str(")\n");
            base_prompt.push_str(&ctx_file.content);
        }

        // 4b. Appended (not replacing) project instructions from config, per the config contract.
        if let Some(rel) = thunder_cfg.system_prompt_file() {
            if let Some(ws) = &self.workspace_root {
                let rel_path = std::path::Path::new(rel);
                let has_traversal = rel_path.is_absolute()
                    || rel_path
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                if has_traversal {
                    warn!(
                        system_prompt_file = %rel,
                        "Rejected system_prompt_file containing absolute path or parent directory traversal"
                    );
                } else {
                    let thunder_dir = ws.join(".thunder");
                    let path = thunder_dir.join(rel_path);
                    let canon_ok = match (path.canonicalize(), ws.canonicalize()) {
                        (Ok(canon_file), Ok(canon_ws)) => canon_file.starts_with(&canon_ws),
                        _ => true,
                    };
                    if !canon_ok {
                        warn!(
                            path = %path.display(),
                            "Rejected system_prompt_file resolving outside the workspace"
                        );
                    } else {
                        match tokio::fs::read_to_string(&path).await {
                            Ok(text) if !text.trim().is_empty() => {
                                base_prompt.push_str("\n\n");
                                base_prompt.push_str(text.trim_end());
                            }
                            Ok(_) => {}
                            Err(err) => warn!(
                                path = %path.display(),
                                error = %err,
                                "Configured system_prompt_file could not be read"
                            ),
                        }
                    }
                }
            }
        }
        if let Some(ws) = &self.workspace_root {
            base_prompt.push_str("\n\n### Workspaces\n");
            base_prompt.push_str(&format!(
                "Primary workspace: {}\n(relative paths resolve here)\n",
                ws.display()
            ));
            if self.extra_roots.is_empty() {
                base_prompt.push_str(
                    "Work inside the primary workspace. A path outside it is not forbidden — the \
                     user is asked before anything outside the working area is written — so prefer \
                     a path inside the workspace whenever there is a choice.",
                );
            } else {
                base_prompt
                    .push_str("Referenced repositories (read/write allowed, absolute paths):\n");
                for root in &self.extra_roots {
                    base_prompt.push_str(&format!("- {}\n", root.display()));
                }
                base_prompt.push_str(
                    "Work inside the primary workspace or one of the referenced repositories above. \
                     Paths outside them are not forbidden — the user is asked before anything \
                     outside the working area is written — so prefer a path inside one of them \
                     whenever there is a choice. \n\
                     When working in a repository, address files by absolute path (read_file / write_file) or pass its \
                     directory as `cwd` for shell commands; prefer write_file over shell redirections for edits.",
                );
            }
        }
        let combined_system_prompt = active_set.build_combined_system_prompt(Some(&base_prompt));
        let mut agent_cfg = self.config.clone();
        if let Some(ref ws) = self.workspace_root {
            agent_cfg.workspace_dir = Some(ws.clone());
        }
        agent_cfg.extra_workspace_roots = self.extra_roots.clone();
        thunder_cfg.apply_to(&mut agent_cfg);
        // Tags every tool call, which is how a plugin's reverse RPC finds this
        // run's services instead of another run's.
        agent_cfg.route = Some(route.clone());
        agent_cfg.system_prompt = Some(combined_system_prompt);
        if let Some(ref tl) = options.thinking_level {
            agent_cfg.thinking_level = Some(tl.clone());
        }

        let mut resolved_client = options.custom_client.clone();
        if let Some(spec) = self.provider_registry.resolve_ref(&self.active_model) {
            // Dynamically inject the model's authentic context window limit into pruning config
            agent_cfg.pruning.max_context_tokens = spec.context_window;
            info!(
                session_id = %session_id,
                model = %spec.selection_id(),
                provider = %spec.provider,
                api = ?spec.api,
                context_window = spec.context_window,
                available = spec.available,
                "Resolved model specification"
            );
            if resolved_client.is_none() {
                match client_for(spec, self.config.request_timeout_ms) {
                    Ok(client) => resolved_client = Some(client),
                    Err(err) => error!(
                        model = %self.active_model.selection_id(),
                        error = %err,
                        "Failed to instantiate LLM client for resolved model"
                    ),
                }
            }
        }

        // Install the code capability pack: this host is a coding agent, so it
        // gets the shell/file tools, the atomic writes, and the scope that tells
        // the kernel which paths are inside the working area. The generic loop
        // knows none of that.
        let code_scope = thunder_agent_pack_code::CodeScope::from_agent_config(
            &agent_cfg,
            self.workspace_root.as_deref(),
        );
        let run_cfg = agent_cfg.clone();
        let mut agent = AgentLoop::new(agent_cfg).with_id(format!("root_{}", session_id));

        if let Some(client) = resolved_client {
            agent = agent.with_custom_client(client);
        } else if options.custom_client.is_none() {
            warn!(
                model = %self.active_model.selection_id(),
                "No matching model found in provider registry. AgentLoop will use UnconfiguredLLMClient"
            );
        }

        // Load declarative plugins (built-ins + MCP servers + config filtering).
        // If `McpPlugin` is already active in `active_set`, it manages workspace and
        // declarative MCP servers with connection pooling, so skip unpooled spawning here.
        let mcp_plugin_active = active_set.plugin_ids().iter().any(|id| id == "mcp");
        let loaded = crate::loader::PluginLoader::load_plugins_with_mcp(
            &thunder_cfg,
            self.workspace_root.as_deref(),
            options.register_builtins,
            !mcp_plugin_active,
        )
        .await;

        let disabled_set: std::collections::HashSet<String> = thunder_cfg
            .plugins()
            .and_then(|p| p.disabled.as_ref())
            .map(|list| list.iter().cloned().collect())
            .unwrap_or_default();

        // ── The host owns the tools and the judge ────────────────────────
        let mut registry = ToolRegistry::new(
            run_cfg.max_tool_output_bytes,
            std::time::Duration::from_millis(run_cfg.request_timeout_ms),
        )
        .with_scratchpad(agent.scratchpad().clone());

        for tool in active_set.collect_tools() {
            if !crate::loader::matches_plugin_id(&disabled_set, &tool.name()) {
                registry.register(tool);
            }
        }
        for plugin in active_set.collect_tool_plugins() {
            if !crate::loader::matches_plugin_id(&disabled_set, plugin.id()) {
                registry.register_plugin(plugin);
            }
        }
        for plugin in loaded.tool_plugins {
            registry.register_plugin(plugin);
        }

        // The judge. Always installed, including in `yolo`: the mode is what
        // decides whether a call asks, and one code path for every mode means
        // there is no configuration in which the floor is the only check.
        let mut executor = ToolExecutor::new(registry.clone())
            .with_tool_scope(Arc::new(code_scope) as Arc<dyn ToolScope>)
            .with_tool_gate(Some(Arc::clone(&mode_plugin) as Arc<dyn ToolGate>))
            .with_host_ui(Arc::clone(&ui))
            .with_trace_ids(run_cfg.session_id.clone(), run_cfg.route.clone());

        for interceptor in loaded.interceptors {
            executor = executor.with_interceptor(interceptor);
        }
        for interceptor in active_set.collect_interceptors() {
            if interceptor.id() == "mode_interceptor"
                || crate::loader::matches_plugin_id(&disabled_set, interceptor.id())
            {
                continue;
            }
            executor = executor.with_interceptor(interceptor);
        }

        agent = agent.with_tools_factory(Arc::new(StaticToolsFactory::new(
            registry,
            executor.clone(),
        )));

        // Hand the plugin host an invoker that dispatches through *this*
        // pipeline — not the bare registry, so a plugin-initiated call is
        // judged exactly like a model-initiated one.
        {
            let mut slot = tool_slot.write().await;
            *slot = Some(Arc::new(
                ExecutorToolInvoker::new(executor.clone())
                    .with_turn(1)
                    .with_cancellation(cancel.clone())
                    .with_ui(Arc::clone(&ui)),
            ));
        }
        // Phase two of the plugin handshake: the pipeline now exists, so plugins
        // that expose tools can be given an invoker for *this* run.
        active_set.dispatch_ready(&ctx).await?;

        // Forward the host's pause gate so the unit can park at tool boundaries.
        if let Some(gate) = &options.pause_gate {
            agent = agent.with_pause_gate(Arc::clone(gate));
        }

        // Forward the steering / follow-up queues so the host can inject user
        // input into this run at turn boundaries.
        if let Some(queues) = &options.steer_queues {
            agent = agent.with_steer_queues(Arc::clone(queues));
        }

        // 5. Start AgentLoop with full context input
        let handle = agent
            .start(context_input, Some(cancel))
            .map_err(|e| PluginError::ExecutionFailed(e.to_string()))?;

        let agent_id = handle.agent_id().to_string();

        Ok(RootRunHandle {
            agent_id,
            selection,
            handle,
            active_set,
            ctx,
            dispatch_drained: None,
        })
    }

    /// Run to completion.
    pub async fn run(
        &self,
        input: impl Into<ContextInput>,
        options: RootRunOptions,
    ) -> Result<RootRunResult, PluginError> {
        let mut handle = self.execute(input, options).await?;
        // Taking the stream is what installs the proxy that dispatches
        // `on_event` to plugins and forwards status/metrics. Skipping it — as
        // this convenience path used to — meant `run()` produced a correct
        // result but every plugin saw a silent run, and `join()` had nothing
        // to wait for.
        if let Some(mut events) = handle.take_events() {
            tokio::spawn(async move { while events.recv().await.is_some() {} });
        }
        handle
            .join()
            .await
            .map_err(|e| PluginError::ExecutionFailed(e.to_string()))
    }
}

async fn clean_stale_scratch_dirs(root: &std::path::Path, max_age: std::time::Duration) {
    if !root.exists() {
        return;
    }
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };
    let now = std::time::SystemTime::now();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > max_age {
                            let _ = tokio::fs::remove_dir_all(&path).await;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod scratch_cleanup_tests {
    use super::clean_stale_scratch_dirs;

    #[tokio::test]
    async fn stale_dirs_are_removed_and_fresh_ones_kept() {
        let root =
            std::env::temp_dir().join(format!("thunder_scratch_cleanup_{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        let stale = root.join("sess_stale");
        let fresh = root.join("sess_fresh");
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::create_dir_all(&fresh).await.unwrap();
        tokio::fs::write(stale.join("junk.log"), b"x")
            .await
            .unwrap();

        // A zero max-age treats every existing dir as stale; the fresh dir is
        // created after the scan so it survives. Use a large age for "fresh":
        // call twice with distinct thresholds to prove both branches.
        clean_stale_scratch_dirs(&root, std::time::Duration::from_secs(0)).await;
        assert!(!stale.exists(), "stale dir must be removed");
        assert!(!fresh.exists(), "zero max-age removes every dir too");

        // Recreate and confirm a huge max-age removes nothing.
        tokio::fs::create_dir_all(&stale).await.unwrap();
        clean_stale_scratch_dirs(&root, std::time::Duration::from_secs(365 * 86400)).await;
        assert!(
            stale.exists(),
            "a fresh-looking dir survives a wide max-age"
        );

        let _ = tokio::fs::remove_dir_all(&root).await;
    }
}
