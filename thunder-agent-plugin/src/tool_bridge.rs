use crate::process::SidecarManager;
use crate::protocol::ToolMeta;
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tracing::info;

pub struct TsToolBridge {
    tool_meta: ToolMeta,
    sidecar: Arc<SidecarManager>,
}

impl TsToolBridge {
    pub fn new(tool_meta: ToolMeta, sidecar: Arc<SidecarManager>) -> Self {
        Self { tool_meta, sidecar }
    }

    pub fn tool_name(&self) -> &str {
        &self.tool_meta.name
    }

    pub fn plugin_id(&self) -> &str {
        &self.tool_meta.plugin_id
    }
}

#[async_trait]
impl AgentTool for TsToolBridge {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            &self.tool_meta.name,
            format!(
                "[Plugin:{}] {}",
                self.tool_meta.plugin_id, self.tool_meta.description
            ),
            self.tool_meta.parameters.clone(),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        info!(
            tool = %self.tool_meta.name,
            plugin = %self.tool_meta.plugin_id,
            turn = ctx.turn,
            "Executing TypeScript tool call via bridge"
        );

        if ctx.cancellation_token.is_cancelled() {
            return Err("Execution cancelled before TS tool invocation".to_string());
        }

        let context_json = serde_json::json!({
            "callId": ctx.tool_call_id,
            "turn": ctx.turn,
        });

        self.sidecar
            .execute_tool(&self.tool_meta.name, args, context_json)
            .await
    }
}
