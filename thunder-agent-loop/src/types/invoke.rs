//! Calling a tool on someone else's initiative.
//!
//! The loop executes tools when the *model* asks. A plugin sometimes needs to
//! reach a tool of its own — the TypeScript side's `ctx.callTool` is exactly
//! this. That is a privileged capability, and the design goal here is that it is
//! **not** a way around anything.
//!
//! # Why this is not a registry lookup
//!
//! The tempting implementation is "find the tool in the registry and call it".
//! That would route plugin calls around the onion, so a plugin could write a
//! file or run a shell command with no path jail, no transaction, and — critically
//! — no approval prompt. [`PipelineToolInvoker`] instead runs the call through
//! the *same* pipeline as a model-initiated one, so:
//!
//! * the capability tier still applies (a read-only run cannot be escalated);
//! * the approval gate still applies, and now names the plugin;
//! * path jail, transaction and resource guards all still apply.
//!
//! # What a plugin cannot do
//!
//! * call a tool the run's tier forbids;
//! * get a privileged call approved without the user seeing that a *plugin* is
//!   asking ([`crate::types::tool::ToolExecutionContext::caller`]);
//! * bypass the jail or the transaction layer.
//!
//! What it can do is spend the run's own authority — which is the honest
//! description of "a plugin running inside the agent".

use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use crate::types::ui::{HostUi, NotifyLevel, UiRequest, UiResponse, UiSource};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Who is asking, and in what run.
#[derive(Debug, Clone, Default)]
pub struct ToolInvocationContext {
    /// Stable id of the calling plugin, surfaced in approval dialogs.
    pub plugin_id: String,
    /// Session the call belongs to, for logging and panel routing.
    pub session_id: Option<String>,
    /// Turn the host is on, so a plugin's call can be correlated with the turn
    /// that let it run.
    pub turn: Option<usize>,
}

/// Runs one tool call on behalf of a plugin.
///
/// Implementations must apply the same policy layers a model-initiated call
/// goes through. Returning `Ok` is a statement that the call really ran.
#[async_trait]
pub trait ToolInvoker: Send + Sync {
    async fn invoke(
        &self,
        tool: &str,
        args: serde_json::Value,
        ctx: &ToolInvocationContext,
    ) -> Result<String, String>;
}

/// The default invoker: refuses everything.
///
/// Used by embedders that run no agent of their own. A refusal here is a
/// *feature*, not a stub — it means "this host has no tool pipeline to call
/// into", which is exactly what the plugin needs to hear.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullToolInvoker;

#[async_trait]
impl ToolInvoker for NullToolInvoker {
    async fn invoke(
        &self,
        tool: &str,
        _args: serde_json::Value,
        ctx: &ToolInvocationContext,
    ) -> Result<String, String> {
        Err(format!(
            "'{tool}' is not callable from a plugin in this host: there is no agent tool pipeline \
             to dispatch it. {}",
            if ctx.plugin_id.is_empty() {
                String::new()
            } else {
                format!("(requested by '{}')", ctx.plugin_id)
            }
        ))
    }
}

/// A live slot holding the current run's invoker.
///
/// Filled by the host *after* the agent's tools are registered, which is why it
/// is a slot rather than a value: the plugin host boots before the agent exists.
///
/// # Concurrency
///
/// Process-wide, so two concurrent runs overwrite each other — the same
/// limitation the permission tier and host UI carry, and for the same reason:
/// the sidecar is a single shared process. It is safe to rely on for *policy*
/// (a lower-privileged run's pipeline is what gets called, and it refuses what
/// that run may not do) and must not be relied on for *attribution* of a
/// privileged approval across concurrent runs.
pub type ToolInvokerSlot = Arc<tokio::sync::RwLock<Option<Arc<dyn ToolInvoker>>>>;

/// An empty slot, for hosts with no agent.
pub fn empty_tool_invoker_slot() -> ToolInvokerSlot {
    Arc::new(tokio::sync::RwLock::new(None))
}

/// Runs plugin-initiated calls through a real pipeline.
///
/// A snapshot of the executor taken once every tool is registered. Registering a
/// tool *after* this is created is not reflected — hosts build this at the end of
/// setup, so that is not a reachable case, but it is why this type is not
/// exposed for mid-run mutation.
pub struct PipelineToolInvoker {
    executor: crate::tools::executor::ToolExecutor,
    turn: usize,
    timeout: Option<Duration>,
    /// Notifies the user that a plugin acted on its own initiative.
    ui: Option<Arc<dyn HostUi>>,
    /// Cancellation token from the owning run so plugin tool invocations abort promptly.
    cancellation_token: Option<tokio_util::sync::CancellationToken>,
    counter: std::sync::atomic::AtomicU64,
}

impl PipelineToolInvoker {
    pub fn new(executor: crate::tools::executor::ToolExecutor) -> Self {
        Self {
            executor,
            turn: 0,
            timeout: None,
            ui: None,
            cancellation_token: None,
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }

    pub fn with_turn(mut self, turn: usize) -> Self {
        self.turn = turn;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn with_cancellation(mut self, token: tokio_util::sync::CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Announce plugin-initiated calls to the user.
    ///
    /// Only fires for calls that pass the pipeline, so a refused one is reported
    /// by the layer that refused it — with the right reason.
    pub fn with_ui(mut self, ui: Arc<dyn HostUi>) -> Self {
        self.ui = Some(ui);
        self
    }
}

#[async_trait]
impl ToolInvoker for PipelineToolInvoker {
    async fn invoke(
        &self,
        tool: &str,
        args: serde_json::Value,
        ctx: &ToolInvocationContext,
    ) -> Result<String, String> {
        let seq = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tool_call_id = format!("plugin_call_{}_{}", ctx.plugin_id, seq);

        let call = ToolCall::new_function(
            tool_call_id.clone(),
            tool,
            // A plugin sends an object; serialise it so the pipeline sees the
            // same shape it would from a provider.
            serde_json::to_string(&args).map_err(|e| e.to_string())?,
        );

        // The whole point: tagged with `caller` so the approval dialog and any
        // other human-facing surface can attribute the request.
        let tool_ctx = ToolExecutionContext {
            tool_call_id,
            turn: ctx.turn.unwrap_or(self.turn),
            cancellation_token: self.cancellation_token.clone().unwrap_or_default(),
            caller: Some(ctx.plugin_id.clone()),
            // The plugin's own session, so downstream shared services resolve
            // the same run the host would.
            route: ctx.session_id.clone(),
            // Left unset on purpose: `execute_with_context` stamps the tool's
            // declared effect and the run's event sink before the pipeline runs.
            ..Default::default()
        };

        // `execute_with_context`, not `execute_one`: the latter would rebuild the
        // context and drop `caller`, which is exactly the field that tells an
        // approval dialog a plugin asked.
        let result: ToolExecutionResult = self
            .executor
            .execute_with_context(&call, tool_ctx, self.timeout)
            .await;

        if result.is_error {
            // Surface the layer's own message. It already explains *why* (tier,
            // jail, user refusal) and carries the telemetry notice, so inventing
            // a second explanation here would only blur the cause.
            return Err(result.output);
        }

        if let Some(ui) = &self.ui {
            if !ctx.plugin_id.is_empty() {
                ui.notify(
                    UiSource::Plugin,
                    &format!("Plugin '{}' called {tool}", ctx.plugin_id),
                    NotifyLevel::Info,
                );
            }
        }

        if result.truncated {
            Ok(format!(
                "{}\n[truncated: {} of {} bytes; the rest is in the scratchpad]",
                result.output,
                result.output.len(),
                result.original_bytes
            ))
        } else {
            Ok(result.output)
        }
    }
}

/// Whether a host has any tool to call at all, for a friendlier refusal.
pub async fn invoker_available(slot: &ToolInvokerSlot) -> bool {
    slot.read().await.is_some()
}

/// Ask a UI whether a plugin may act, when a host wants an extra confirmation
/// beyond the approval gate.
///
/// Provided because some hosts want plugin-initiated calls to be visible even in
/// a mode that does not otherwise prompt. The answer is fail-closed like every
/// other dialog.
pub async fn confirm_plugin_action(
    ui: &dyn HostUi,
    plugin_id: &str,
    tool: &str,
    detail: &str,
) -> bool {
    let response = ui
        .request(
            UiSource::Plugin,
            UiRequest::Confirm {
                title: format!("Plugin {plugin_id} wants to run {tool}"),
                message: detail.to_string(),
                timeout_ms: None,
            },
        )
        .await;
    response.confirmed()
}

/// Unwrap a response that is known to carry text.
pub fn response_text(response: &UiResponse) -> Option<&str> {
    response.text()
}
