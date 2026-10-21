use crate::tools::{handle_exit_plan_mode, handle_write_plan, ModeFlipCallback};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use thunder_protocol::{ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider};

pub struct PlanToolProvider {
    mode_flip_callback: Option<ModeFlipCallback>,
}

impl PlanToolProvider {
    pub fn new() -> Self {
        Self {
            mode_flip_callback: None,
        }
    }

    pub fn with_flip_callback(mut self, cb: ModeFlipCallback) -> Self {
        self.mode_flip_callback = Some(cb);
        self
    }

    pub fn shared(cb: Option<ModeFlipCallback>) -> Arc<Self> {
        let mut p = Self::new();
        if let Some(cb) = cb {
            p = p.with_flip_callback(cb);
        }
        Arc::new(p)
    }
}

impl Default for PlanToolProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolProvider for PlanToolProvider {
    fn provider_id(&self) -> &str {
        "plan"
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        Ok(vec![
            ToolDescriptor::new_core(
                "write_plan",
                "Write or overwrite the project implementation plan markdown in .thunder/plans/<session_id>.md.",
                json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "Full markdown text of the implementation plan (goals, approach, checklist, verification)."
                        }
                    },
                    "required": ["content"]
                }),
            ),
            ToolDescriptor::new_core(
                "exit_plan_mode",
                "Signal that the plan is complete, request approval, and transition the session from plan mode into active mode.",
                json!({
                    "type": "object",
                    "properties": {
                        "explanation": {
                            "type": "string",
                            "description": "Short explanation of the completed plan for the user review."
                        }
                    }
                }),
            ),
        ])
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        let logical = name.strip_prefix("core__").unwrap_or(name);
        match logical {
            "write_plan" => handle_write_plan(&arguments, ctx)
                .await
                .map_err(|e| ToolExecutionError::NonZeroExit { code: 1, stderr: e }),
            "exit_plan_mode" => {
                handle_exit_plan_mode(&arguments, ctx, self.mode_flip_callback.as_ref())
                    .await
                    .map_err(|e| ToolExecutionError::NonZeroExit { code: 1, stderr: e })
            }
            other => Err(ToolExecutionError::ToolNotFound(other.to_string())),
        }
    }
}
