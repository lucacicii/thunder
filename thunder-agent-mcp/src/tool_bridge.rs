use crate::client::McpClient;
use crate::protocol::McpTool;
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tracing::{debug, info};

/// Dynamic bridge wrapping a remote MCP Tool into a local `AgentTool` recognized by `thunder-agent-loop`.
pub struct McpToolBridge {
    server_name: String,
    tool_info: McpTool,
    client: Arc<McpClient>,
    exposed_name: String,
}

impl McpToolBridge {
    pub fn new(server_name: impl Into<String>, tool_info: McpTool, client: Arc<McpClient>) -> Self {
        let s_name = server_name.into();
        let exposed_name = format!("mcp_{}_{}", s_name.replace('-', "_"), tool_info.name.replace('-', "_"));
        Self {
            server_name: s_name,
            tool_info,
            client,
            exposed_name,
        }
    }

    pub fn with_custom_name(mut self, name: impl Into<String>) -> Self {
        self.exposed_name = name.into();
        self
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn original_tool_name(&self) -> &str {
        &self.tool_info.name
    }
}

#[async_trait]
impl AgentTool for McpToolBridge {
    fn definition(&self) -> ToolDefinition {
        let desc = self
            .tool_info
            .description
            .clone()
            .unwrap_or_else(|| format!("MCP tool '{}' from server '{}'", self.tool_info.name, self.server_name));

        ToolDefinition::new_function(
            &self.exposed_name,
            format!("[MCP:{}] {desc}", self.server_name),
            self.tool_info.input_schema.clone(),
        )
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolExecutionContext) -> Result<String, String> {
        info!(
            tool = %self.exposed_name,
            original = %self.tool_info.name,
            server = %self.server_name,
            turn = ctx.turn,
            "Executing MCP tool call via bridge"
        );

        if ctx.cancellation_token.is_cancelled() {
            return Err("Execution cancelled before MCP tool invocation".to_string());
        }

        let res = self
            .client
            .call_tool(&self.tool_info.name, Some(args))
            .await
            .map_err(|e| format!("MCP tool execution failed on server '{}': {e}", self.server_name))?;

        let output_text = res.plain_text();
        debug!(tool = %self.exposed_name, output_len = output_text.len(), is_error = ?res.is_error, "MCP tool execution finished");

        if res.is_error.unwrap_or(false) {
            Err(output_text)
        } else {
            Ok(output_text)
        }
    }
}
