//! The reference [`Tools`] implementation.

use crate::executor::ToolExecutor;
use crate::registry::ToolRegistry;
use crate::tool_policy::{AgentTool, ToolExecutionContext};
use futures_util::future::BoxFuture;
use std::sync::Arc;
use thunder_agent_loop::seam::{
    RunContext, ToolCallContext, ToolOutcome, ToolSpec, Tools, ToolsFactory,
};
use thunder_agent_types::message::ToolCall;

pub struct RegistryTools {
    registry: ToolRegistry,
    executor: ToolExecutor,
    /// The trace slot this run installed, if any.
    trace_generation: Option<u64>,
}

impl RegistryTools {
    /// Build tools that do not own a run's trace slot (tests, one-shot tools).
    pub fn new(registry: ToolRegistry, executor: ToolExecutor) -> Self {
        Self {
            registry,
            executor,
            trace_generation: None,
        }
    }

    /// Build the tools for one run, owning the trace slot `generation`.
    pub fn for_run(registry: ToolRegistry, executor: ToolExecutor, trace_generation: u64) -> Self {
        Self {
            registry,
            executor,
            trace_generation: Some(trace_generation),
        }
    }
}

impl Drop for RegistryTools {
    /// The per-run `Tools` object owns the executor's trace slot: dropping it
    /// releases the run's trace sink, and with it the run's event sender.
    ///
    /// This matters beyond memory. A host's event proxy waits for the run's
    /// event channel to close before it reports the run drained; a sink left in
    /// the shared slot kept a sender alive forever, so every run's tail hung.
    fn drop(&mut self) {
        if let Some(generation) = self.trace_generation {
            self.executor.clear_run_trace(generation);
        }
    }
}

impl Tools for RegistryTools {
    fn specs(&self) -> Vec<ToolSpec> {
        self.registry
            .get_definitions()
            .into_iter()
            .map(|definition| {
                let concurrency = self
                    .registry
                    .get(&definition.function.name)
                    .map(|t| t.concurrency())
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
            let r = self
                .executor
                .run_tool(call, execution_context(&ctx), ctx.timeout)
                .await;
            let mut outcome: ToolOutcome = r.into();
            if outcome.original_bytes == 0 && !outcome.is_error {
                outcome.original_bytes = outcome.output.len();
            }
            outcome
        })
    }
}

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

pub struct StaticToolsFactory {
    registry: ToolRegistry,
    executor: ToolExecutor,
}

impl StaticToolsFactory {
    pub fn new(registry: ToolRegistry, executor: ToolExecutor) -> Self {
        Self { registry, executor }
    }
}

impl ToolsFactory for StaticToolsFactory {
    fn for_run(&self, run: &RunContext) -> Arc<dyn Tools> {
        let generation = self.executor.install_run_trace(Arc::clone(&run.trace));
        Arc::new(RegistryTools::for_run(
            self.registry.clone(),
            self.executor.clone(),
            generation,
        ))
    }
}

pub fn plain_tools(
    tools: Vec<Arc<dyn AgentTool>>,
    max_output_bytes: usize,
    timeout: std::time::Duration,
) -> Arc<dyn Tools> {
    let mut registry = ToolRegistry::new(max_output_bytes, timeout);
    for tool in tools {
        registry.register(tool);
    }
    let executor = ToolExecutor::new(registry.clone());
    Arc::new(RegistryTools::new(registry, executor))
}

pub fn single_tool(
    tool: Arc<dyn AgentTool>,
    max_output_bytes: usize,
    timeout: std::time::Duration,
) -> Arc<dyn Tools> {
    plain_tools(vec![tool], max_output_bytes, timeout)
}
