use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::types::policy::ToolEffect;

/// Sink for events a tool or middleware wants to surface to the host.
///
/// The kernel owns the loop, not the meaning of a call, so it cannot know that
/// "this bash command rewrote two files" or "this form submission failed
/// validation" deserves an event. It only knows how to *carry* one. A host (or a
/// capability pack) that does know emits through the sink parked on the
/// execution context, and the loop forwards it as
/// [`crate::types::event::AgentEvent::Custom`] without interpreting the payload.
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

#[derive(Debug, Clone)]
pub struct ToolExecutionContext {
    pub tool_call_id: String,
    pub turn: usize,
    pub cancellation_token: CancellationToken,
    /// Who initiated this call, when it did not come from the model.
    ///
    /// `None` means the model asked for it — the normal case. A plugin invoking
    /// a tool on its own initiative sets this to its own id, so anything that
    /// surfaces the call to a human (notably the approval gate) can say *who* is
    /// asking. Without it a plugin's request is indistinguishable from the
    /// model's, and a user approving "Run bash" has no way to know a plugin
    /// asked rather than the assistant.
    pub caller: Option<String>,
    /// Which run this call belongs to, for services that are shared across runs.
    ///
    /// The TypeScript sidecar is a single Node process serving every concurrent
    /// run, so a reverse RPC from a plugin has no way to tell whose authority it
    /// is spending. Carrying the route on the call lets the host look up *that
    /// run's* permission tier, host UI and tool pipeline — instead of whichever
    /// run happened to initialise last.
    ///
    /// This is a correctness field, not a label: the pipeline it selects holds
    /// the workspace root and the path jail, so misrouting it would let one run's
    /// plugin write inside another run's workspace.
    pub route: Option<String>,
    /// What this call can do to the world, resolved by the executor from the
    /// tool's own [`AgentTool::effect`] declaration.
    ///
    /// Middlewares judge this instead of re-deriving it from the tool name. A
    /// name is a convention; the declaration is a contract, and it is the only
    /// thing that survives a tool called anything at all.
    ///
    /// `None` means nobody resolved it (a hand-built context, a direct
    /// middleware call); consumers then fall back to the name heuristic rather
    /// than assuming the call is harmless.
    pub effect: Option<ToolEffect>,
    /// Sink for business-defined events; see [`ToolEventSink`]. `None` means the
    /// host installed no sink, and a tool that has something to report simply
    /// has nowhere to send it.
    pub event_sink: Option<Arc<dyn ToolEventSink>>,
}

impl Default for ToolExecutionContext {
    fn default() -> Self {
        Self {
            tool_call_id: String::new(),
            turn: 0,
            // A fresh, never-cancelled token: an unowned context has no owner to
            // cancel it. Callers that have a real signal set it explicitly.
            cancellation_token: CancellationToken::new(),
            caller: None,
            route: None,
            // Unresolved: consumers fall back to the name heuristic, which is
            // never weaker than treating an unknown tool as a mutation.
            effect: None,
            event_sink: None,
        }
    }
}

impl ToolExecutionContext {
    /// Tag this call as plugin-initiated, for human-facing surfaces.
    pub fn with_caller(mut self, caller: impl Into<String>) -> Self {
        self.caller = Some(caller.into());
        self
    }

    /// Tag this call as belonging to a run.
    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = Some(route.into());
        self
    }

    /// A short "who is asking" label, empty when the model is the caller.
    pub fn caller_label(&self) -> &str {
        self.caller.as_deref().unwrap_or("")
    }
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
    pub fn with_telemetry(
        mut self,
        notice: crate::tools::middleware::telemetry::SystemNotice,
    ) -> Self {
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

    /// What this tool can do to the world.
    ///
    /// Defaults to the name-based classification so a tool that says nothing is
    /// no worse off than before. A tool that knows its own semantics should
    /// override this: it is the difference between the loop *guessing* and the
    /// tool *declaring*.
    fn effect(&self) -> ToolEffect {
        ToolEffect::of(&self.definition().function.name)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String>;
}
