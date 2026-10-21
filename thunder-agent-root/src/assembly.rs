//! Explicit host assembly: three lists instead of a plugin registry.
//!
//! A host used to be built from `ThunderPlugin` objects. That indirection
//! bought nothing: every "plugin" in this crate is first-party code, and the
//! single trait forced prompt text, tools and event observers into one shape
//! while handing each of them a context object wide enough to reach everything.
//!
//! The three things are genuinely different, so they are three lists:
//!
//! * [`PromptSection`] — text that goes into the system prompt.
//! * [`ToolPack`] — tools the model may call.
//! * [`Observer`] — code that watches the run's events.
//!
//! Per-run setup stays an inherent `prepare(&RunEnv)` on the few pieces that
//! need it (memory and skills read the workspace; mode binds the session). The
//! host calls it explicitly before assembling.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use thunder_agent_loop::types::event::AgentEvent;
use thunder_agent_loop::AgentRunResult;
use thunder_agent_policy::plugin::ToolPlugin;
use thunder_agent_policy::tool_policy::AgentTool;
use thunder_agent_policy::ui::HostUi;
use tokio_util::sync::CancellationToken;

/// What a run hands to the pieces that need per-run state.
#[derive(Clone)]
pub struct RunEnv {
    pub session_id: String,
    pub workspace: Option<PathBuf>,
    pub route: Option<String>,
    pub prompt: Option<String>,
    /// The host's interaction surface for this run.
    pub ui: Arc<dyn HostUi>,
    pub cancellation: CancellationToken,
}

impl std::fmt::Debug for RunEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunEnv")
            .field("session_id", &self.session_id)
            .field("workspace", &self.workspace)
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}

impl RunEnv {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            workspace: None,
            route: None,
            prompt: None,
            ui: Arc::new(thunder_agent_policy::ui::NullHostUi),
            cancellation: CancellationToken::new(),
        }
    }
}

/// One block of system-prompt text.
pub trait PromptSection: Send + Sync {
    /// Stable identifier, also what `plugins.disabled` matches.
    fn id(&self) -> &str;

    /// Lower renders first. The base prompt always renders at 0.
    fn order(&self) -> i32 {
        100
    }

    /// The text, or `None` when this run does not warrant a section.
    fn render(&self) -> Option<String>;
}

/// Something that watches a run.
pub trait Observer: Send + Sync {
    fn id(&self) -> &str;

    /// One event from the run's stream.
    ///
    /// `env` is per-run, so one observer instance may serve concurrent runs
    /// without having to remember which session it was bound to last.
    ///
    /// Async because two of the built-in observers keep their state behind an
    /// async lock (a conversation is loaded from disk).
    fn on_event<'a>(
        &'a self,
        event: &'a AgentEvent,
        env: &'a RunEnv,
    ) -> futures_util::future::BoxFuture<'a, ()>;

    /// The run settled. Default: nothing to do.
    fn on_finish<'a>(
        &'a self,
        _result: &'a AgentRunResult,
        _env: &'a RunEnv,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// Per-run setup for the few pieces that need it.
pub trait Prepared: Send + Sync {
    /// Stable identifier, also what `plugins.disabled` matches.
    fn id(&self) -> &str;

    fn prepare<'a>(
        &'a self,
        env: &'a RunEnv,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
}

/// One unit of tool provision.
#[derive(Clone)]
pub struct ToolPack {
    /// Stable identifier, also what `plugins.disabled` matches.
    pub id: String,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub tool_plugins: Vec<Arc<dyn ToolPlugin>>,
}

impl ToolPack {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            tools: Vec::new(),
            tool_plugins: Vec::new(),
        }
    }

    pub fn with_tool(mut self, tool: Arc<dyn AgentTool>) -> Self {
        self.tools.push(tool);
        self
    }

    pub fn with_tools(mut self, tools: Vec<Arc<dyn AgentTool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    pub fn with_tool_plugin(mut self, plugin: Arc<dyn ToolPlugin>) -> Self {
        self.tool_plugins.push(plugin);
        self
    }
}

/// The host's assembled surface for one run.
#[derive(Default, Clone)]
pub struct Assembly {
    sections: Vec<Arc<dyn PromptSection>>,
    packs: Vec<ToolPack>,
    observers: Vec<Arc<dyn Observer>>,
    prepared: Vec<Arc<dyn Prepared>>,
}

impl Assembly {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_section(mut self, section: Arc<dyn PromptSection>) -> Self {
        self.sections.push(section);
        self
    }

    pub fn with_pack(mut self, pack: ToolPack) -> Self {
        self.packs.push(pack);
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn Observer>) -> Self {
        self.observers.push(observer);
        self
    }

    pub fn with_prepared(mut self, prepared: Arc<dyn Prepared>) -> Self {
        self.prepared.push(prepared);
        self
    }

    /// Drop everything whose id the user disabled.
    pub fn without_disabled(mut self, disabled: &HashSet<String>) -> Self {
        if disabled.is_empty() {
            return self;
        }
        self.sections.retain(|s| !disabled.contains(s.id()));
        self.packs.retain(|p| !disabled.contains(&p.id));
        self.observers.retain(|o| !disabled.contains(o.id()));
        self.prepared.retain(|p| !disabled.contains(p.id()));
        self
    }

    /// Run every per-run setup hook, in registration order.
    pub async fn prepare(&self, env: &RunEnv) -> Result<(), String> {
        for piece in &self.prepared {
            piece.prepare(env).await?;
        }
        Ok(())
    }

    /// `base` plus every section, in `order()` then registration order.
    pub fn prompt(&self, base: &str) -> String {
        let mut sections: Vec<&Arc<dyn PromptSection>> = self.sections.iter().collect();
        sections.sort_by_key(|s| s.order());

        let mut out = String::from(base);
        for section in sections {
            let Some(text) = section.render() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            if !out.ends_with("\n\n") {
                out.push_str("\n\n");
            }
            out.push_str(text.trim_end());
        }
        out
    }

    /// Every id this assembly carries, deduplicated, in registration order.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for id in self
            .sections
            .iter()
            .map(|s| s.id())
            .chain(self.packs.iter().map(|p| p.id.as_str()))
            .chain(self.observers.iter().map(|o| o.id()))
        {
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.to_string());
            }
        }
        ids
    }

    pub fn has_pack(&self, id: &str) -> bool {
        self.packs.iter().any(|p| p.id == id)
    }

    pub fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        self.packs
            .iter()
            .flat_map(|p| p.tools.iter().cloned())
            .collect()
    }

    pub fn tool_plugins(&self) -> Vec<Arc<dyn ToolPlugin>> {
        self.packs
            .iter()
            .flat_map(|p| p.tool_plugins.iter().cloned())
            .collect()
    }

    pub async fn dispatch_event(&self, event: &AgentEvent, env: &RunEnv) {
        for observer in &self.observers {
            observer.on_event(event, env).await;
        }
    }

    pub async fn dispatch_finish(&self, result: &AgentRunResult, env: &RunEnv) {
        for observer in &self.observers {
            observer.on_finish(result, env).await;
        }
    }
}
