use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use thunder_agent_loop::{
    AgentLoop, AgentTool, ToolDefinition, ToolExecutionContext,
};

/// Wrap another complete A unit as a tool.
///
/// Nested multi-agent lives in B, not inside A's runtime. The callee still
/// owns its closed `run()` loop.
pub struct DelegateTool {
    name: String,
    description: String,
    factory: Arc<dyn Fn() -> AgentLoop + Send + Sync>,
}

impl DelegateTool {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        factory: impl Fn() -> AgentLoop + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            factory: Arc::new(factory),
        }
    }
}

#[async_trait]
impl AgentTool for DelegateTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            &self.name,
            &self.description,
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Subtask for the delegated agent unit"
                    }
                },
                "required": ["task"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'task'".to_string())?;

        let agent = (self.factory)();
        let handle = agent
            .start(task.to_string(), Some(ctx.cancellation_token.clone()))
            .map_err(|e| e.to_string())?;
        let result = handle.join().await.map_err(|e| e.to_string())?;

        match result.final_content {
            Some(text) if !text.is_empty() => Ok(text),
            _ => Ok(format!(
                "Delegated agent `{}` finished ({:?}) with no final content",
                result.agent_id, result.finish_reason
            )),
        }
    }
}
