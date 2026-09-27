use crate::error::PluginError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::types::config::Permission;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::invoke::ToolInvokerSlot;
use thunder_agent_loop::types::policy::{PermissionMode, SessionPolicy};
use thunder_agent_loop::types::tool::AgentTool;
use thunder_agent_loop::types::ui::HostUi;
use thunder_agent_loop::AgentRunResult;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginCapability {
    MemoryPersistence,
    ToolProvider,
    SkillProvider,
    McpProvider,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TriggerSpec {
    pub keywords: Vec<String>,
    pub auto_always: bool,
    pub description_for_llm: String,
}

impl TriggerSpec {
    pub fn new(keywords: Vec<&str>, description_for_llm: &str) -> Self {
        Self {
            keywords: keywords.into_iter().map(|s| s.to_lowercase()).collect(),
            auto_always: false,
            description_for_llm: description_for_llm.to_string(),
        }
    }

    pub fn always() -> Self {
        Self {
            keywords: Vec::new(),
            auto_always: true,
            description_for_llm: "Always active plugin".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub capabilities: Vec<PluginCapability>,
    pub triggers: TriggerSpec,
}

impl PluginManifest {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            version: version.into(),
            capabilities: Vec::new(),
            triggers: TriggerSpec::default(),
        }
    }

    pub fn with_capability(mut self, cap: PluginCapability) -> Self {
        self.capabilities.push(cap);
        self
    }

    pub fn with_triggers(mut self, triggers: TriggerSpec) -> Self {
        self.triggers = triggers;
        self
    }
}

/// The host's user-interaction surface for this run.
///
/// Defaults to [`thunder_agent_loop::types::ui::NullHostUi`], so an embedded
/// host without a panel still behaves correctly: every dialog resolves as
/// cancelled, which callers must read as "denied".
///
/// Handed over as a trait object, so it is excluded from the derived `Debug`
/// (implementations are transports, not data).
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
    /// Capability tier of the run this context belongs to.
    ///
    /// The host is the authority on permissions; plugins inherit. Anything a
    /// plugin does outside the loop's tool pipeline (a TypeScript plugin's
    /// `ctx.exec()` / `ctx.fs.writeFile()`) is gated by this value, so it must
    /// reflect the *current* run rather than a process-wide default.
    pub permission: Permission,
    /// The host's user-interaction surface for this run.
    ///
    /// Defaults to [`thunder_agent_loop::types::ui::NullHostUi`], so an embedded
    /// host without a panel still behaves correctly: every dialog resolves as
    /// cancelled, which callers must read as "denied".
    pub ui: HostUiHandle,
    /// Identifies this run among concurrent runs.
    ///
    /// Services shared across runs — the TypeScript sidecar above all — are
    /// keyed by this, so a plugin call is authorised against the run that made
    /// it rather than whichever run registered last.
    pub route: Option<String>,
    /// The run's permission policy.
    ///
    /// Plugins that can reach execution need the whole policy, not a tier: the
    /// tier alone would leave `ctx.exec` outside the mode and the remembered
    /// rules, which is the split this refactor exists to close.
    pub policy: Option<Arc<SessionPolicy>>,

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
            permission: Permission::default(),
            ui: HostUiHandle(Arc::new(thunder_agent_loop::types::ui::NullHostUi)),
            route: None,
            policy: None,
            tools: ToolInvokerHandle(thunder_agent_loop::types::invoke::empty_tool_invoker_slot()),
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

    /// Set the capability tier inherited by this run's plugins.
    pub fn with_permission(mut self, permission: Permission) -> Self {
        self.permission = permission;
        self
    }

    /// Attach the host UI this run's plugins raise dialogs through.
    pub fn with_ui(mut self, ui: Arc<dyn HostUi>) -> Self {
        self.ui = HostUiHandle(ui);
        self
    }

    /// Attach the run's permission policy.
    pub fn with_policy(mut self, policy: Arc<SessionPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The run's policy, defaulting to a permissive one for embedders that have
    /// not adopted it. Constructed fresh per call so the tier and mode are never
    /// shared between two plugins of the same run by accident.
    pub fn policy(&self) -> Arc<SessionPolicy> {
        self.policy
            .clone()
            .unwrap_or_else(|| SessionPolicy::new(Permission::Bash, PermissionMode::default()))
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

    /// The tool-invoker slot, for handing to a plugin host.
    pub fn tool_slot(&self) -> ToolInvokerSlot {
        Arc::clone(&self.tools.0)
    }

    /// The host UI, as a shareable trait object.
    pub fn ui(&self) -> Arc<dyn HostUi> {
        Arc::clone(&self.ui.0)
    }
}

#[async_trait]
pub trait ThunderPlugin: Send + Sync {
    /// Return the manifest metadata of the plugin.
    fn manifest(&self) -> &PluginManifest;

    /// Optional tools to register directly into the core `AgentLoop`.
    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        vec![]
    }

    /// Optional system prompt fragment to inject into the core context.
    fn system_prompt_contribution(&self) -> Option<String> {
        None
    }

    /// Lifecycle hook: Called before the root execution starts.
    async fn on_init(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    /// Lifecycle hook: Called once every tool is registered and the agent's
    /// pipeline is final.
    ///
    /// Split from [`ThunderPlugin::on_init`] because a plugin host needs two
    /// different things at two different times: the run's *identity and policy*
    /// before the agent exists, and its *tool pipeline* after. A plugin that
    /// exposes other tools needs both, keyed by [`PluginContext::route`].
    async fn on_run_ready(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Ok(())
    }

    /// Lifecycle hook: Called whenever an `ObservedEvent` occurs in `AgentLoop`.
    async fn on_event(&self, _event: &ObservedEvent, _ctx: &PluginContext) {}

    /// Lifecycle hook: Called after the root execution completes.
    async fn on_finish(
        &self,
        _result: &AgentRunResult,
        _ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        Ok(())
    }
}
