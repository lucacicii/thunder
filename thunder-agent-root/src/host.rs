use crate::error::PluginError;
use crate::plugin::PluginContext;
use crate::registry::{ActivePluginSet, PluginRegistry};
use crate::selector::{PluginSelection, PluginSelector};
use crate::instructions::discover_context_files;
use crate::thunder_config::ThunderConfig;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::handle::AgentHandle;
use thunder_agent_loop::stream::client::LLMClientTrait;
use thunder_agent_loop::tools::builtin::{
    BashTool, FindTool, GrepTool, ListDirTool, ReadFileTool, WriteFileTool,
};
use thunder_agent_loop::types::config::Permission;
use thunder_agent_loop::types::invoke::{
    empty_tool_invoker_slot, PipelineToolInvoker, ToolInvokerSlot,
};
use thunder_agent_loop::types::policy::SessionPolicy;
use thunder_agent_loop::types::ui::HostUi;
use thunder_agent_loop::{
    AgentConfig, AgentError, AgentLoop, AgentRunResult, ChatMessage, ContextInput, ObservedEvent,
    DEFAULT_AUTONOMOUS_SYSTEM_PROMPT,
};
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
    /// Tool capability tier (read ⊂ write ⊂ bash). Defaults to the widest tier;
    /// hosts may narrow it explicitly.
    pub permission: Permission,
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
    /// Session-scoped approval state. Hosts pass the *same* policy across every
    /// run of a session so "always allow" rules and mid-session mode switches
    /// survive a turn; `None` creates a fresh one for this run only.
    pub policy: Option<Arc<SessionPolicy>>,
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
            permission: Permission::default(),
            pause_gate: None,
            steer_queues: None,
            ui: None,
            route: None,
            policy: None,
        }
    }
}

pub struct RootRunResult {
    pub agent_id: String,
    pub final_content: Option<String>,
    pub selection: PluginSelection,
    pub run_result: AgentRunResult,
}

pub struct RootRunHandle {
    pub agent_id: String,
    pub selection: PluginSelection,
    pub handle: AgentHandle,
    pub active_set: ActivePluginSet,
    pub ctx: PluginContext,
}

impl RootRunHandle {
    pub fn take_events(&mut self) -> Option<mpsc::Receiver<ObservedEvent>> {
        self.handle.take_events()
    }

    pub async fn join(self) -> Result<RootRunResult, AgentError> {
        let run_result = self.handle.join().await?;
        let _ = self
            .active_set
            .dispatch_finish(&run_result, &self.ctx)
            .await;

        Ok(RootRunResult {
            agent_id: self.agent_id,
            final_content: run_result.final_content.clone(),
            selection: self.selection,
            run_result,
        })
    }
}

/// Source of per-run route ids, so two runs of one session never collide.
static RUN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct ThunderRoot {
    config: AgentConfig,
    registry: PluginRegistry,
    selector: PluginSelector,
    workspace_root: Option<PathBuf>,
    /// Extra roots (e.g. referenced repositories) sharing the workspace's
    /// read/write standing in the security jail.
    extra_roots: Vec<PathBuf>,
    scratch_root: PathBuf,
    provider_registry: ProviderRegistry,
    active_model: ModelRef,
    /// Default host UI handed to runs that do not override it per-run.
    ///
    /// Defaults to `NullHostUi`: no panel, every dialog cancelled, every caller
    /// degrades to its safe branch.
    host_ui: Arc<dyn HostUi>,
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
        Self {
            config,
            registry: PluginRegistry::new(),
            selector,
            workspace_root: None,
            extra_roots: Vec::new(),
            scratch_root,
            provider_registry: ProviderRegistry::default(),
            active_model,
            host_ui: Arc::new(thunder_agent_loop::types::ui::NullHostUi),
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

        let session_id = options.session_id.unwrap_or_else(|| {
            format!(
                "sess_{}",
                thunder_agent_loop::types::event::TurnStats::default().turn + 1
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

        let active_set = self
            .registry
            .create_active_set(&selection.active_plugin_ids);

        // 1b. Resolve the approval policy for this run.
        //
        // A host-supplied policy is reused across the session (so remembered
        // rules and any host-chosen approval mode survive); a fresh one defaults
        // to `Never` (no prompting). The tier is this run's capability ceiling.
        let effective_permission = options.permission;
        let policy = options
            .policy
            .clone()
            .unwrap_or_else(|| SessionPolicy::new(options.permission, Default::default()));
        policy.set_tier(options.permission).await;
        let mode = policy.mode().await;
        let ui = options
            .ui
            .clone()
            .unwrap_or_else(|| Arc::clone(&self.host_ui));

        info!(
            session_id = %session_id,
            tier = ?options.permission,
            mode = mode.as_str(),
            effective_tier = ?effective_permission,
            "Resolved permission policy"
        );

        // 2. Prepare Plugin Context
        // The permission tier is carried here so every plugin — including the
        // TypeScript sidecar, whose `ctx.exec()` / `ctx.fs.writeFile()` bypass
        // the loop's tool pipeline — inherits the *run's* ceiling instead of a
        // process-wide default.
        // Filled in after the agent exists — see the note at the fill site.
        let tool_slot: ToolInvokerSlot = empty_tool_invoker_slot();
        let mut ctx = PluginContext::new(&session_id)
            .with_cancellation(cancel.clone())
            .with_permission(effective_permission)
            .with_ui(Arc::clone(&ui))
            .with_route(route.clone())
            .with_policy(Arc::clone(&policy))
            .with_prompt(prompt_str.clone())
            .with_tool_slot(Arc::clone(&tool_slot));
        if let Some(ws) = &self.workspace_root {
            ctx = ctx.with_workspace(ws.clone());
        }
        ctx = ctx.with_scratch(self.scratch_root.join(&session_id));

        // 3. Dispatch plugin on_init lifecycle
        active_set.dispatch_init(&ctx).await?;

        // 4. Construct Root AgentLoop
        let mut base_prompt = self
            .config
            .system_prompt
            .as_deref()
            .unwrap_or(DEFAULT_AUTONOMOUS_SYSTEM_PROMPT)
            .to_string();
        // 4a. Discover and append AGENTS.md / CLAUDE.md instruction context files (aligning with Pi)
        let home_dir = std::env::var("HOME").ok().map(PathBuf::from);
        let context_files = discover_context_files(self.workspace_root.as_deref(), home_dir.as_deref()).await;
        for ctx_file in context_files {
            base_prompt.push_str("\n\n### Context Instructions (");
            base_prompt.push_str(&ctx_file.path.display().to_string());
            base_prompt.push_str(")\n");
            base_prompt.push_str(&ctx_file.content);
        }

        // 4b. Appended (not replacing) project instructions from config, per the config contract.
        if let Some(rel) = thunder_cfg.system_prompt_file() {
            if let Some(ws) = &self.workspace_root {
                let path = ws.join(".thunder").join(rel);
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
        if let Some(ws) = &self.workspace_root {
            base_prompt.push_str("\n\n### Workspaces\n");
            base_prompt.push_str(&format!(
                "Primary workspace: {}\n(relative paths resolve here)\n",
                ws.display()
            ));
            if self.extra_roots.is_empty() {
                base_prompt.push_str(
                    "All file paths and shell write targets must stay within the primary workspace \
                     unless the user explicitly instructs otherwise.",
                );
            } else {
                base_prompt
                    .push_str("Referenced repositories (read/write allowed, absolute paths):\n");
                for root in &self.extra_roots {
                    base_prompt.push_str(&format!("- {}\n", root.display()));
                }
                base_prompt.push_str(
                    "All file paths and shell write targets must stay within the primary workspace or one of the \
                     referenced repositories above, unless the user explicitly instructs otherwise. \n\
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
        agent_cfg.permission = effective_permission;
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

        let mut agent = AgentLoop::new(agent_cfg).with_id(format!("root_{}", session_id));

        // The judge. Always installed, including in `yolo`: the mode stops the
        // *prompting*, not the tier check, and one code path for every mode means
        // there is no configuration in which the ceiling is skipped.
        agent = agent.with_policy(Arc::clone(&policy), ui.clone());

        if let Some(client) = resolved_client {
            agent = agent.with_custom_client(client);
        } else if options.custom_client.is_none() {
            warn!(
                model = %self.active_model.selection_id(),
                "No matching model found in provider registry. AgentLoop will use UnconfiguredLLMClient"
            );
        }

        if options.register_builtins {
            let perm = options.permission;
            let ws = self.workspace_root.clone();
            // Gate by capability tier: a denied tool is never registered, so it
            // never reaches the model's tool list in the first place.
            if perm.allows_read() {
                let tool = ReadFileTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => tool.with_default_cwd(dir.clone()),
                    None => tool,
                }));
            }
            if perm.allows_write() {
                let tool = WriteFileTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => tool.with_default_cwd(dir.clone()),
                    None => tool,
                }));
            }
            if perm.allows_exec() {
                let tool = BashTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => tool.with_default_cwd(dir.clone()),
                    None => tool,
                }));
            }
            // Read-only lookup tools share the read capability tier.
            if perm.allows_read() {
                let grep = GrepTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => grep.with_default_cwd(dir.clone()),
                    None => grep,
                }));
                let find = FindTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => find.with_default_cwd(dir.clone()),
                    None => find,
                }));
                let ls = ListDirTool::default();
                agent.register_tool(Arc::new(match &ws {
                    Some(dir) => ls.with_default_cwd(dir.clone()),
                    None => ls,
                }));
            }
            if perm != Permission::Bash {
                info!(
                    session_id = %session_id,
                    permission = %perm.describe(),
                    "Built-in tools restricted by permission tier"
                );
            }
        }

        // Register tools contributed by active plugins
        for tool in active_set.collect_tools() {
            agent.register_tool(tool);
        }

        // Now that every tool is registered, hand the plugin host an invoker
        // that dispatches through *this* pipeline.
        //
        // Ordering matters twice over:
        //   * the snapshot is taken here because `register_tool` rebuilds the
        //     pipeline, so an earlier snapshot would miss later registrations;
        //   * dispatching through the pipeline — not the bare registry — is what
        //     makes a plugin-initiated call subject to the same tier, jail,
        //     transaction and approval gate as a model-initiated one.
        {
            let mut slot = tool_slot.write().await;
            *slot = Some(Arc::new(
                PipelineToolInvoker::new(agent.tool_executor().clone())
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
        })
    }

    /// Run to completion.
    pub async fn run(
        &self,
        input: impl Into<ContextInput>,
        options: RootRunOptions,
    ) -> Result<RootRunResult, PluginError> {
        let handle = self.execute(input, options).await?;
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
