use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::types::policy::ToolEffect;
use crate::types::ui::UiHandle;

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
    /// This is a correctness field, not a label: it is how a plugin's reverse
    /// tool call reaches that run's own services — its workspace, its scratchpad,
    /// its trace — so misrouting it would let one run's plugin act inside
    /// another run's workspace.
    pub route: Option<String>,
    /// What this call can do to the world, resolved by the executor from the
    /// tool's own [`AgentTool::effect`] declaration.
    ///
    /// The guard and the host's gate judge this instead of re-deriving it from
    /// the tool name. A name is a convention; the declaration is a contract, and
    /// it is the only thing that survives a tool called anything at all.
    ///
    /// `None` means nobody resolved it (a hand-built context); consumers then
    /// fall back to the name heuristic rather than assuming the call is
    /// harmless.
    pub effect: Option<ToolEffect>,
    /// Sink for business-defined events; see [`ToolEventSink`]. `None` means the
    /// host installed no sink, and a tool that has something to report simply
    /// has nowhere to send it.
    pub event_sink: Option<Arc<dyn ToolEventSink>>,
    /// The host's panel, stamped by the executor the same way the event sink
    /// is: a tool that needs to ask the user something (the plan approval above
    /// all) raises it through here rather than through its own plumbing.
    /// `None` means nobody stamped it — a direct `execute` call — and the tool
    /// degrades to no panel, which for a dialog means the fail-closed branch.
    pub ui: Option<UiHandle>,
    /// Current invocation depth (0 for direct model call, incremented on sub-invocations).
    pub depth: usize,
    /// Maximum allowed depth before refusing sub-invocations (default: 3).
    pub max_depth: usize,
    /// Call chain for cycle/deadlock detection and attribution: `["root_tool", "sub_tool"]`.
    pub call_chain: Vec<String>,
    /// Sub-tool invocation provider for safe, decoupled inter-tool calls.
    pub sub_invoker: Option<Arc<dyn crate::types::plugin::SubToolInvoker>>,
    /// Streaming progress reporter interface for long-running operations.
    pub progress_reporter: Option<Arc<dyn ProgressReporter>>,
}

/// Streaming progress reporter interface (aligned with Anthropic MCP `notifications/progress`).
#[async_trait]
pub trait ProgressReporter: Send + Sync + std::fmt::Debug {
    /// Report execution progress.
    ///
    /// - `progress`: The current progress value (e.g. 10.0 or 0.5).
    /// - `total`: Optional total number of steps or units (e.g. 100.0 or 1.0).
    /// - `message`: Optional human-readable message describing the current step.
    async fn report_progress(&self, progress: f64, total: Option<f64>, message: Option<String>);
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
            ui: None,
            depth: 0,
            max_depth: 3,
            call_chain: Vec::new(),
            sub_invoker: None,
            progress_reporter: None,
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

    /// Set current call depth.
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// Set maximum allowed call depth before refusing calls.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Set sub-tool invoker.
    pub fn with_sub_invoker(
        mut self,
        invoker: Arc<dyn crate::types::plugin::SubToolInvoker>,
    ) -> Self {
        self.sub_invoker = Some(invoker);
        self
    }

    /// Attach a progress reporter.
    pub fn with_progress_reporter(mut self, reporter: Arc<dyn ProgressReporter>) -> Self {
        self.progress_reporter = Some(reporter);
        self
    }

    /// Report execution progress if a reporter is installed.
    pub async fn report_progress(
        &self,
        progress: f64,
        total: Option<f64>,
        message: Option<impl Into<String>>,
    ) {
        if let Some(reporter) = &self.progress_reporter {
            reporter
                .report_progress(progress, total, message.map(Into::into))
                .await;
        }
    }

    /// A short "who is asking" label, empty when the model is the caller.
    pub fn caller_label(&self) -> &str {
        self.caller.as_deref().unwrap_or("")
    }

    /// Safely invoke another tool through the kernel proxy.
    ///
    /// Automatically enforces:
    /// 1. Maximum invocation depth limit (`depth < max_depth`) to prevent stack overflow.
    /// 2. Cycle detection on `call_chain` to prevent infinite recursion / deadlocks.
    /// 3. Proper inheritance of cancellation, caller attribution, and proxy chain.
    pub async fn invoke(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<crate::types::plugin::CallToolResult, String> {
        if self.depth >= self.max_depth {
            return Err(format!(
                "Refused tool invocation: Max invocation depth ({}) exceeded. Cannot call '{}'.",
                self.max_depth, tool
            ));
        }

        if self.call_chain.iter().any(|c| c == tool) {
            return Err(format!(
                "Refused tool invocation: Cycle detected in call chain: {:?} -> '{}'. Aborting to prevent infinite recursion.",
                self.call_chain, tool
            ));
        }

        let invoker = self.sub_invoker.as_ref().ok_or_else(|| {
            format!("No sub-tool invoker configured in ToolExecutionContext to invoke '{tool}'.")
        })?;

        let mut next_chain = self.call_chain.clone();
        next_chain.push(tool.to_string());

        let child_ctx = ToolExecutionContext {
            tool_call_id: format!("{}_{}_{}", self.tool_call_id, tool, self.depth + 1),
            turn: self.turn,
            cancellation_token: self.cancellation_token.clone(),
            caller: Some(
                self.call_chain
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "model".into()),
            ),
            route: self.route.clone(),
            effect: None,
            event_sink: self.event_sink.clone(),
            ui: self.ui.clone(),
            depth: self.depth + 1,
            max_depth: self.max_depth,
            call_chain: next_chain,
            sub_invoker: self.sub_invoker.clone(),
            progress_reporter: self.progress_reporter.clone(),
        };

        invoker.invoke_sub_tool(tool, args, &child_ctx).await
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
    pub telemetry: Option<crate::tools::notice::SystemNotice>,
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
    pub fn with_telemetry(mut self, notice: crate::tools::notice::SystemNotice) -> Self {
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

    /// Name of the tool as exposed in its definition.
    fn name(&self) -> String {
        self.definition().function.name
    }

    /// What this tool can do to the world.
    ///
    /// Defaults to the name-based classification so a tool that says nothing is
    /// no worse off than before. A tool that knows its own semantics should
    /// override this: it is the difference between the loop *guessing* and the
    /// tool *declaring*.
    fn effect(&self) -> ToolEffect {
        ToolEffect::of(&self.definition().function.name)
    }

    /// How this tool may be scheduled relative to its batch peers.
    ///
    /// The loop only *schedules*; it never judges. A tool that must not run
    /// concurrently with its siblings (single writer, serial port, a locking
    /// editor) says [`Concurrency::Sequential`](crate::seam::Concurrency::Sequential)
    /// here and the whole batch runs in `tool_calls` order.
    fn concurrency(&self) -> crate::seam::Concurrency {
        crate::seam::Concurrency::Parallel
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String>;
}
