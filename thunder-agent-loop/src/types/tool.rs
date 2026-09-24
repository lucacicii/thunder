use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    pub fn new_function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
                strict: None,
            },
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub tool_call_id: String,
    pub turn: usize,
    pub cancellation_token: CancellationToken,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub output: String,
    pub is_error: bool,
    pub truncated: bool,
    pub original_bytes: usize,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<crate::tools::middleware::telemetry::SystemNotice>,
}

impl ToolExecutionResult {
    pub fn success(output: String, duration: Duration) -> Self {
        let bytes = output.len();
        Self {
            output,
            is_error: false,
            truncated: false,
            original_bytes: bytes,
            duration_ms: duration.as_millis() as u64,
            telemetry: None,
        }
    }

    pub fn error(error_msg: String, duration: Duration) -> Self {
        let bytes = error_msg.len();
        Self {
            output: error_msg,
            is_error: true,
            truncated: false,
            original_bytes: bytes,
            duration_ms: duration.as_millis() as u64,
            telemetry: None,
        }
    }

    /// Appends a structured telemetry notice so the LLM is informed with ground truth.
    pub fn with_telemetry(mut self, notice: crate::tools::middleware::telemetry::SystemNotice) -> Self {
        let md = notice.format_markdown();
        if self.output.trim().is_empty() {
            self.output = md;
        } else {
            self.output = format!("{}\n\n{}", self.output, md);
        }
        self.original_bytes = self.output.len();
        self.telemetry = Some(notice);
        self
    }
}

#[async_trait]
pub trait AgentTool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: serde_json::Value, ctx: &ToolExecutionContext) -> Result<String, String>;
}
