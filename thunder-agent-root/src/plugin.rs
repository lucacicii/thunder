use crate::error::PluginError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::types::event::ObservedEvent;
use thunder_agent_loop::types::tool::AgentTool;
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

#[derive(Debug, Clone)]
pub struct PluginContext {
    pub session_id: String,
    pub workspace_dir: Option<PathBuf>,
    pub scratch_dir: Option<PathBuf>,
    pub cancellation_token: CancellationToken,
}

impl PluginContext {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            workspace_dir: None,
            scratch_dir: None,
            cancellation_token: CancellationToken::new(),
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
