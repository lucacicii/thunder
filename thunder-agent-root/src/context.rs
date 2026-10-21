use crate::plugins::metrics::MetricsPlugin;
use crate::plugins::mode::{Mode, ModePlugin};
use crate::plugins::status::StatusPlugin;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_policy::invoke::ToolInvokerSlot;
use thunder_agent_policy::ui::HostUi;
use tokio_util::sync::CancellationToken;

/// The host's user-interaction surface for this run.
pub struct HostUiHandle(pub Arc<dyn HostUi>);

impl std::fmt::Debug for HostUiHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostUiHandle(..)")
    }
}

impl Clone for HostUiHandle {
    fn clone(&self) -> Self {
        HostUiHandle(Arc::clone(&self.0))
    }
}

#[derive(Debug, Clone)]
pub struct PluginContext {
    pub session_id: String,
    pub workspace_dir: Option<PathBuf>,
    pub scratch_dir: Option<PathBuf>,
    pub cancellation_token: CancellationToken,
    /// The mode of the run this context belongs to.
    ///
    /// The host is the authority on modes; plugins inherit, so a plugin that
    /// frames its own guidance to the model can mention what the user chose.
    pub mode: Mode,
    /// The host's user-interaction surface for this run.
    ///
    /// Defaults to [`thunder_agent_policy::ui::NullHostUi`], so an embedded
    /// host without a panel still behaves correctly: every dialog resolves as
    /// cancelled, which callers must read as "denied".
    pub ui: HostUiHandle,
    /// Identifies this run among concurrent runs.
    ///
    /// Services shared across runs — the TypeScript sidecar above all — are
    /// keyed by this, so a plugin call is authorised against the run that made
    /// it rather than whichever run registered last.
    pub route: Option<String>,
    /// The run's mode plugin, when the host installed one.
    ///
    /// Held by the context so a plugin that can reach execution (through
    /// `tools`, above) shares the *session's* rules and mode rather than a
    /// per-call snapshot.
    pub mode_plugin: Option<Arc<ModePlugin>>,
    /// The user input this run was started with, flattened to text.
    ///
    /// The user's turn never appears in the event stream, so a plugin that
    /// mirrors the session (the conversation store above all) has no other way
    /// to record what the user actually asked. `None` when the host started the
    /// run without a user turn.
    pub prompt: Option<String>,

    /// The run's tool invoker, for `ctx.callTool`.
    ///
    /// Empty at construction because the plugin host is initialised *before* the
    /// agent exists: the invoker can only be built once every tool is
    /// registered. The host fills it, then dispatches
    /// [`ThunderPlugin::on_run_ready`].
    ///
    /// Wrapped so the derived `Debug` keeps working — a trait object is not
    /// `Debug`.
    pub tools: ToolInvokerHandle,
    pub metrics_plugin: Option<MetricsPlugin>,
    /// The run's execution-status plugin, when the host installed one.
    ///
    /// Held here for the same reason as `metrics_plugin`: a host that publishes
    /// this run's status onto its own wire needs the plugin's snapshot, and it
    /// reaches that through the run's context rather than through the registry
    /// (which is shared across runs and keyed by plugin id).
    pub status_plugin: Option<StatusPlugin>,
}

/// Newtype over [`ToolInvokerSlot`], excluded from `Debug` like [`HostUiHandle`].
#[derive(Clone)]
pub struct ToolInvokerHandle(pub ToolInvokerSlot);

impl std::fmt::Debug for ToolInvokerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolInvokerHandle(..)")
    }
}

impl PluginContext {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            workspace_dir: None,
            scratch_dir: None,
            cancellation_token: CancellationToken::new(),
            mode: Mode::default(),
            ui: HostUiHandle(Arc::new(thunder_agent_policy::ui::NullHostUi)),
            route: None,
            mode_plugin: None,
            prompt: None,
            tools: ToolInvokerHandle(thunder_agent_policy::invoke::empty_tool_invoker_slot()),
            metrics_plugin: None,
            status_plugin: None,
        }
    }

    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace_dir = Some(path);
        self
    }

    pub fn with_scratch(mut self, path: PathBuf) -> Self {
        self.scratch_dir = Some(path);
        self
    }

    pub fn with_cancellation(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = token;
        self
    }

    /// Set the mode inherited by this run's plugins.
    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }

    /// Attach the host UI this run's plugins raise dialogs through.
    pub fn with_ui(mut self, ui: Arc<dyn HostUi>) -> Self {
        self.ui = HostUiHandle(ui);
        self
    }

    /// Attach the run's mode plugin.
    pub fn with_mode_plugin(mut self, mode_plugin: Arc<ModePlugin>) -> Self {
        self.mode_plugin = Some(mode_plugin);
        self
    }

    /// The run's mode plugin, defaulting to a permissive one.
    ///
    /// Deliberately `Yolo`, not `Mode::default()`: this fallback serves hosts
    /// that installed no mode plugin at all, and for such a host the honest
    /// default is what the kernel would do with no gate — allow everything the
    /// guard allows. Defaulting to a mode that asks, combined with a null UI,
    /// would lock every plugin's call behind a dialog nobody can answer.
    pub fn gate(&self) -> Arc<ModePlugin> {
        self.mode_plugin
            .clone()
            .unwrap_or_else(|| ModePlugin::new(Mode::Yolo))
    }

    /// Attach the tool-invoker slot this run's plugins dispatch through.
    pub fn with_tool_slot(mut self, tools: ToolInvokerSlot) -> Self {
        self.tools = ToolInvokerHandle(tools);
        self
    }

    /// Identify this run, for services shared across concurrent runs.
    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = Some(route.into());
        self
    }

    /// Attach the user input this run was started with.
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    /// The run's user input, if it had one.
    pub fn prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    /// The tool-invoker slot, for handing to a plugin host.
    pub fn tool_slot(&self) -> ToolInvokerSlot {
        Arc::clone(&self.tools.0)
    }

    /// The host UI, as a shareable trait object.
    pub fn ui(&self) -> Arc<dyn HostUi> {
        Arc::clone(&self.ui.0)
    }

    pub fn with_metrics_plugin(mut self, metrics_plugin: MetricsPlugin) -> Self {
        self.metrics_plugin = Some(metrics_plugin);
        self
    }

    pub fn metrics_plugin(&self) -> Option<&MetricsPlugin> {
        self.metrics_plugin.as_ref()
    }

    /// Attach the run's execution-status plugin.
    pub fn with_status_plugin(mut self, status_plugin: StatusPlugin) -> Self {
        self.status_plugin = Some(status_plugin);
        self
    }

    /// The run's execution-status plugin, when the host installed one.
    pub fn status_plugin(&self) -> Option<&StatusPlugin> {
        self.status_plugin.as_ref()
    }
}
