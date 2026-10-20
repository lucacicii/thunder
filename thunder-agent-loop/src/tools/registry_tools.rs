//! The reference [`Tools`] implementation.
//!
//! This is the *adapter*, not the loop: it wraps today's `ToolRegistry` +
//! `ToolExecutor` (approval, guard, scope, traces, scratchpad) and presents them
//! through the seam. It deliberately lives outside `seam.rs` so the seam itself
//! depends on nothing but the wire types — that is what makes the seam movable
//! into a policy-free kernel crate.
//!
//! A host that wants its own policy supplies its own `Tools` through
//! [`AgentLoop::with_tools`](crate::loop_engine::engine::AgentLoop::with_tools).

use crate::seam::{ToolCallContext, ToolOutcome, ToolSpec, Tools};
use crate::tools::executor::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::ToolExecutionContext;
use futures_util::future::BoxFuture;

/// The reference implementation: today's registry + executor behind the seam.
///
/// Kept in the kernel crate deliberately — it is the *adapter*, not the loop.
/// Moving it to the host is a mechanical relocation; the loop compiles and runs
/// identically with it or with a host-supplied `Tools`.
pub struct RegistryTools {
    registry: ToolRegistry,
    executor: ToolExecutor,
}

impl RegistryTools {
    pub fn new(registry: ToolRegistry, executor: ToolExecutor) -> Self {
        Self { registry, executor }
    }
}

impl Tools for RegistryTools {
    fn specs(&self) -> Vec<ToolSpec> {
        // `ToolRegistry` stores tools in a `BTreeMap`, so this order is stable
        // across runs — the property the prompt cache depends on.
        self.registry
            .get_definitions()
            .into_iter()
            .map(|definition| {
                let concurrency = self
                    .registry
                    .get(&definition.function.name)
                    .map(|tool| tool.concurrency())
                    .unwrap_or_default();
                ToolSpec::new(definition, concurrency)
            })
            .collect()
    }

    fn call<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: ToolCallContext<'a>,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let result = self
                .executor
                .run_tool(call, execution_context(&ctx), ctx.timeout)
                .await;
            let mut outcome: ToolOutcome = result.into();
            // The executor already measured inside the funnel; the seam only
            // needs a sane byte count when the adapter had none to report.
            if outcome.original_bytes == 0 && !outcome.is_error {
                outcome.original_bytes = outcome.output.len();
            }
            outcome
        })
    }
}


/// Translate the seam's borrowed context into the executor's owned one.
///
/// The executor fills the rest in itself (effect declaration, sub-invoker,
/// call chain); this only forwards what the loop measured.
fn execution_context(ctx: &ToolCallContext<'_>) -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: ctx.tool_call_id.to_string(),
        turn: ctx.turn,
        cancellation_token: ctx.cancel.clone(),
        route: ctx.route.map(str::to_string),
        event_sink: ctx.events.clone(),
        ..Default::default()
    }
}
