//! Policy-side tool types: what a registered tool is, and the context it runs in.

use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use thunder_agent_types::tool::{ProgressReporter, ToolDefinition, ToolEventSink};
use thunder_agent_types::policy::ToolEffect;
use crate::ui::UiHandle;

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
    pub sub_invoker: Option<Arc<dyn crate::plugin::SubToolInvoker>>,
    /// Streaming progress reporter interface for long-running operations.
    pub progress_reporter: Option<Arc<dyn ProgressReporter>>,
}

/// Streaming progress reporter interface (aligned with Anthropic MCP `notifications/progress`).

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
        invoker: Arc<dyn crate::plugin::SubToolInvoker>,
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
    ) -> Result<crate::plugin::CallToolResult, String> {
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
    /// editor) says [`Concurrency::Sequential`](thunder_agent_loop::seam::Concurrency::Sequential)
    /// here and the whole batch runs in `tool_calls` order.
    fn concurrency(&self) -> thunder_agent_loop::seam::Concurrency {
        thunder_agent_loop::seam::Concurrency::Parallel
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String>;
}