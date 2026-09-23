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
                "pipeline", "parallel", "council", "orchestra", "review", "planner", "coder",
                "audit", "delegate", "编排", "流水线", "并行", "审查", "多智能体", "分工",
            ],
            "Coordinates multi-agent workflows, sequential pipelines, parallel council reviews, and subagent delegation.",
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
        let base_cfg = self.config.base.clone().unwrap_or_else(|| {
            thunder_agent_loop::AgentConfig::new("gpt-4o")
        });

        // Register a delegate subtask tool
        let delegate_tool = DelegateTool::new(
            "delegate_subtask",
            "Delegate an isolated subtask to a dedicated specialist sub-agent unit",
            move || {
                let mut agent = thunder_agent_loop::AgentLoop::new(base_cfg.clone());
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
