//! The reference [`Tools`] implementation.

use crate::seam::{RunContext, ToolCallContext, ToolOutcome, ToolSpec, Tools, ToolsFactory};
use crate::tools::executor::ToolExecutor;
use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::{AgentTool, ToolExecutionContext};
use futures_util::future::BoxFuture;
use std::sync::Arc;

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
        self.registry.get_definitions().into_iter().map(|definition| {
            let concurrency = self.registry.get(&definition.function.name)
                .map(|t| t.concurrency()).unwrap_or_default();
            ToolSpec::new(definition, concurrency)
        }).collect()
    }

    fn call<'a>(&'a self, call: &'a ToolCall, ctx: ToolCallContext<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let r = self.executor.run_tool(call, execution_context(&ctx), ctx.timeout).await;
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
        self.executor.install_run_trace(Arc::clone(&run.trace));
        Arc::new(RegistryTools::new(self.registry.clone(), self.executor.clone()))
    }
}

pub fn plain_tools(
    tools: Vec<Arc<dyn AgentTool>>,
    max_output_bytes: usize,
    timeout: std::time::Duration,
) -> Arc<dyn Tools> {
    let mut registry = ToolRegistry::new(max_output_bytes, timeout);
    for tool in tools { registry.register(tool); }
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
