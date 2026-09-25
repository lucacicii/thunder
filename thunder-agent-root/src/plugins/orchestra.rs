#[cfg(feature = "orchestra")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::types::tool::AgentTool;
use thunder_orchestra::{DelegateTool, OrchestraConfig, Scheduler};

#[cfg(feature = "orchestra")]
pub struct OrchestraPlugin {
    manifest: PluginManifest,
    config: OrchestraConfig,
    scheduler: Scheduler,
}

#[cfg(feature = "orchestra")]
impl OrchestraPlugin {
    pub fn new(config: OrchestraConfig) -> Self {
        let manifest = PluginManifest::new(
            "orchestra",
            "Thunder Multi-Agent Orchestra",
            "Coordinates multi-agent topologies (Sequential Pipeline, Parallel Council, Auto Router, Delegated Sub-Agents).",
            "0.1.0",
        )
        .with_capability(PluginCapability::MultiAgentOrchestration)
        .with_capability(PluginCapability::ToolProvider)
        .with_triggers(TriggerSpec::new(
            vec![
                "pipeline", "orchestra", "subagent", "subtasks", "fanout",
                "delegate", "编排", "流水线", "多智能体", "分工协作",
            ],
            "Coordinates multi-agent workflows, sequential pipelines, and subagent delegation.",
        ));

        let scheduler = Scheduler::new(config.clone());

        Self {
            manifest,
            config,
            scheduler,
        }
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    pub fn config(&self) -> &OrchestraConfig {
        &self.config
    }
}

#[cfg(feature = "orchestra")]
#[async_trait]
impl ThunderPlugin for OrchestraPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn tools(&self) -> Vec<Arc<dyn AgentTool>> {
        // Honest gating: without a base AgentConfig there is no meaningful
        // sub-agent to delegate to. Register nothing instead of silently
        // guessing a model id the user never configured.
        let base_cfg = match self.config.base.clone() {
            Some(cfg) => cfg,
            None => {
                tracing::warn!(
                    "OrchestraPlugin: no base AgentConfig configured; the `delegate_subtask` \
                     tool is not registered (attach one via 'OrchestraConfig::with_base')"
                );
                return vec![];
            }
        };

        // The delegate sub-agent must resolve its LLM client through the same
        // composition-root factory as `Scheduler::spawn_unit`. It builds its
        // own `AgentLoop` and therefore never inherits the outer agent's
        // client implicitly — without this seam, real-mode delegation dies on
        // the first LLM call with `UnconfiguredLLMClient`.
        let client_factory = self.config.client_factory.clone();

        // Register a delegate subtask tool
        let delegate_tool = DelegateTool::new(
            "delegate_subtask",
            "Delegate an isolated subtask to a dedicated specialist sub-agent unit",
            move || {
                let mut agent = thunder_agent_loop::AgentLoop::new(base_cfg.clone());
                if let Some(factory) = &client_factory {
                    if let Some(client) = factory(&base_cfg) {
                        agent = agent.with_custom_client(client);
                    }
                }
                agent.register_tool(Arc::new(thunder_agent_loop::BashTool::default()));
                agent.register_tool(Arc::new(thunder_agent_loop::ReadFileTool::default()));
                agent.register_tool(Arc::new(thunder_agent_loop::WriteFileTool::default()));
                agent
            },
        );

        vec![Arc::new(delegate_tool)]
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        Some(
            "You have access to the `delegate_subtask` tool to spawn isolated specialist sub-agents for complex or parallel subtasks."
                .to_string(),
        )
    }

    async fn on_init(&self, _ctx: &PluginContext) -> Result<(), PluginError> {
        Ok(())
    }
}
