//! Wire types for tools: the declarations a provider request carries and the
//! result a tool hands back.
//!
//! Everything here is data or a sink trait. `AgentTool` and
//! `ToolExecutionContext` are policy-side and live in the loop crate.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::notice::SystemNotice;

pub trait ToolEventSink: Send + Sync + std::fmt::Debug {
    /// Emit a business-defined event. `kind` is the host's own label (e.g.
    /// `file_change`); `payload` is opaque to the kernel.
    fn emit_custom(&self, kind: &str, payload: serde_json::Value);
}

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
    pub fn new_function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
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


#[async_trait]
pub trait ProgressReporter: Send + Sync + std::fmt::Debug {
    /// Report execution progress.
    ///
    /// - `progress`: The current progress value (e.g. 10.0 or 0.5).
    /// - `total`: Optional total number of steps or units (e.g. 100.0 or 1.0).
    /// - `message`: Optional human-readable message describing the current step.
    async fn report_progress(&self, progress: f64, total: Option<f64>, message: Option<String>);
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    pub output: String,
    pub is_error: bool,
    pub truncated: bool,
    pub original_bytes: usize,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<SystemNotice>,
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
    pub fn with_telemetry(mut self, notice: SystemNotice) -> Self {
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

