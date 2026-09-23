use crate::error::PluginError;
use crate::plugin::PluginContext;
use crate::registry::{ActivePluginSet, PluginRegistry};
use crate::selector::{PluginSelection, PluginSelector};
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::handle::AgentHandle;
use thunder_agent_loop::stream::client::LLMClientTrait;
use thunder_agent_loop::tools::builtin::{BashTool, ReadFileTool, WriteFileTool};
use thunder_agent_loop::{
    AgentConfig, AgentError, AgentLoop, AgentRunResult, ChatMessage, ContextInput, ObservedEvent,
};
use thunder_agent_providers::prelude::{client_for, ModelRef, ProviderRegistry};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

#[derive(Clone)]
pub struct RootRunOptions {
    pub session_id: Option<String>,
    pub use_mock: bool,
    pub custom_client: Option<Arc<dyn LLMClientTrait>>,
    pub cancellation_token: Option<CancellationToken>,
    pub forced_plugins: Option<Vec<String>>,
    pub register_builtins: bool,
    pub thinking_level: Option<String>,
}

impl Default for RootRunOptions {
    fn default() -> Self {
        Self {
            session_id: None,
            use_mock: false,
            custom_client: None,
            cancellation_token: None,
            forced_plugins: None,
            register_builtins: true,
            thinking_level: None,
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
        let _ = self.active_set.dispatch_finish(&run_result, &self.ctx).await;

        Ok(RootRunResult {
            agent_id: self.agent_id,
            final_content: run_result.final_content.clone(),
            selection: self.selection,
            run_result,
        })
    }
}

pub struct ThunderRoot {
    config: AgentConfig,
    registry: PluginRegistry,
    selector: PluginSelector,
    workspace_root: Option<PathBuf>,
    scratch_root: PathBuf,
    provider_registry: ProviderRegistry,
    active_model: ModelRef,
}

impl ThunderRoot {
    pub fn new(config: AgentConfig) -> Self {
        let scratch_root = std::env::temp_dir().join("thunder_root_scratch");
        let selector = PluginSelector::new(Some(config.clone()));
        let active_model = ModelRef::parse(&config.model);
        Self {
            config,
            registry: PluginRegistry::new(),
            selector,
            workspace_root: None,
            scratch_root,
            provider_registry: ProviderRegistry::default(),
            active_model,
        }
    }

    pub async fn with_providers(mut self) -> Self {
        if let Ok(registry) = ProviderRegistry::load_from_sources(
            &thunder_agent_providers::source::ConfigSource::default_chain(self.workspace_root.as_deref()),
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
        self.workspace_root = Some(path);
        self
    }

    pub fn with_scratch_root(mut self, path: PathBuf) -> Self {
        self.scratch_root = path;
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

        let session_id = options
            .session_id
            .unwrap_or_else(|| format!("sess_{}", thunder_agent_loop::types::event::TurnStats::default().turn + 1));

        let cancel = options.cancellation_token.unwrap_or_default();

        // 1. Select active plugins
        let selection = if let Some(forced) = options.forced_plugins {
            PluginSelection {
                active_plugin_ids: forced,
                reason: "Explicitly forced by execution options".to_string(),
                confidence: 1.0,
            }
        } else {
            self.selector
                .select(&prompt_str, &self.registry, options.use_mock)
                .await
        };

        info!(
            session_id = %session_id,
            active_plugins = ?selection.active_plugin_ids,
            reason = %selection.reason,
            "ThunderRoot executing prompt"
        );

        let active_set = self.registry.create_active_set(&selection.active_plugin_ids);

        // 2. Prepare Plugin Context
        let mut ctx = PluginContext::new(&session_id).with_cancellation(cancel.clone());
        if let Some(ws) = &self.workspace_root {
            ctx = ctx.with_workspace(ws.clone());
        }
        ctx = ctx.with_scratch(self.scratch_root.join(&session_id));

        // 3. Dispatch plugin on_init lifecycle
        active_set.dispatch_init(&ctx).await?;

        // 4. Construct Root AgentLoop
        let mut base_prompt = String::from("You are an autonomous engineering assistant powered by Thunder Agent.");
        if let Some(ws) = &self.workspace_root {
            base_prompt.push_str(&format!(
                "\n\n### Current Workspace\nThe active workspace directory is: {}\nAll operations, inspections, file reads/writes, and shell commands must target or execute inside this directory unless specifically instructed otherwise.",
                ws.display()
            ));
        }
        let combined_system_prompt = active_set.build_combined_system_prompt(Some(&base_prompt));
        let mut agent_cfg = self.config.clone();
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

        if let Some(client) = resolved_client {
            agent = agent.with_custom_client(client);
        } else if options.custom_client.is_none() {
            warn!(
                model = %self.active_model.selection_id(),
                "No matching model found in provider registry. AgentLoop will use UnconfiguredLLMClient"
            );
        }

        if options.register_builtins {
            let mut bash = BashTool::default();
            let mut read_file = ReadFileTool::default();
            let mut write_file = WriteFileTool::default();
            if let Some(ws) = &self.workspace_root {
                bash = bash.with_default_cwd(ws.clone());
                read_file = read_file.with_default_cwd(ws.clone());
                write_file = write_file.with_default_cwd(ws.clone());
            }
            agent.register_tool(Arc::new(bash));
            agent.register_tool(Arc::new(read_file));
            agent.register_tool(Arc::new(write_file));
        }

        // Register tools contributed by active plugins
        for tool in active_set.collect_tools() {
            agent.register_tool(tool);
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
