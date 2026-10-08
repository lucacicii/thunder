use crate::tools::middleware::{ToolMiddleware, ToolPipeline};
use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolEventSink, ToolExecutionContext, ToolExecutionResult};
use futures_util::future::join_all;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ExecutedToolResult {
    pub tool_call: ToolCall,
    pub result: ToolExecutionResult,
}

#[derive(Clone)]
pub struct ToolExecutor {
    registry: ToolRegistry,
    pipeline: ToolPipeline,
    /// Per-run event sink, installed by the loop before it executes anything.
    ///
    /// Held behind a lock rather than cloned into each call because the loop
    /// sets it once per run, after this executor was built, and a run has
    /// exactly one sink for its whole lifetime.
    event_sink: Arc<Mutex<Option<Arc<dyn ToolEventSink>>>>,
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        let pipeline = ToolPipeline::from_registry(registry.clone());
        Self {
            registry,
            pipeline,
            event_sink: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_pipeline(registry: ToolRegistry, pipeline: ToolPipeline) -> Self {
        Self {
            registry,
            pipeline,
            event_sink: Arc::new(Mutex::new(None)),
        }
    }

    /// Return this executor with the run's event sink installed.
    ///
    /// Consuming builder, deliberately: the sink belongs to one run, and a
    /// shared `Arc` would leak that run's event sender into every future clone
    /// of this executor — keeping a finished run's event stream open forever.
    /// Swapping the slot on a fresh clone keeps the ownership honest.
    pub fn with_event_sink(mut self, sink: Arc<dyn ToolEventSink>) -> Self {
        self.event_sink = Arc::new(Mutex::new(Some(sink)));
        self
    }

    /// What this call can do to the world: the tool's own declaration when the
    /// tool is registered, otherwise the conservative name-based fallback.
    fn resolve_effect(&self, tool_name: &str) -> crate::types::policy::ToolEffect {
        self.registry
            .get(tool_name)
            .map(|t| t.effect())
            .unwrap_or_else(|| crate::types::policy::ToolEffect::of(tool_name))
    }

    pub fn with_middleware(mut self, mw: Arc<dyn ToolMiddleware>) -> Self {
        self.pipeline.add_middleware(mw);
        self
    }

    pub fn add_middleware(&mut self, mw: Arc<dyn ToolMiddleware>) {
        self.pipeline.add_middleware(mw);
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn pipeline(&self) -> &ToolPipeline {
        &self.pipeline
    }

    /// Execute a single call through the full pipeline with a caller-supplied
    /// context.
    ///
    /// Preferred over [`ToolExecutor::execute_one`] whenever the context carries
    /// something the pipeline must not lose — notably `caller`, which is how an
    /// approval dialog knows a plugin asked rather than the model. Silently
    /// rebuilding the context would strip that attribution.
    pub async fn execute_with_context(
        &self,
        call: &ToolCall,
        mut ctx: ToolExecutionContext,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        // Stamp the tool's declared effect and the run's event sink. Both are
        // the executor's to know, not the caller's: a plugin that builds a
        // context by hand should still be judged by what the tool *is*, and
        // should still be able to report events.
        ctx.effect = Some(self.resolve_effect(&call.function.name));
        if ctx.event_sink.is_none() {
            ctx.event_sink = self.event_sink.lock().clone();
        }
        self.pipeline.execute(call, &ctx, custom_timeout).await
    }

    /// Execute a single call through the full pipeline.
    ///
    /// Same layers, same guards as [`ToolExecutor::execute_all`] — which is
    /// exactly why a plugin can use this without becoming a way around them.
    pub async fn execute_one(
        &self,
        call: &ToolCall,
        turn: usize,
        cancellation_token: CancellationToken,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        let ctx = ToolExecutionContext {
            tool_call_id: call.id.clone(),
            turn,
            cancellation_token,
            ..Default::default()
        };
        self.execute_with_context(call, ctx, custom_timeout).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_all(
        &self,
        tool_calls: &[ToolCall],
        turn: usize,
        cancellation_token: CancellationToken,
        route: Option<String>,
        custom_timeout: Option<Duration>,
    ) -> Vec<ExecutedToolResult> {
        if tool_calls.is_empty() {
            return Vec::new();
        }

        // Shared across the parallel futures, so each one clones the string
        // rather than moving it out of the closure.
        let route = route.map(Arc::new);
        let futures = tool_calls.iter().map(|tc| {
            let route = route.clone();
            let tc_clone = tc.clone();
            let token = cancellation_token.clone();

            async move {
                let ctx = ToolExecutionContext {
                    tool_call_id: tc_clone.id.clone(),
                    turn,
                    cancellation_token: token,
                    route: route.as_ref().map(|r| r.as_str().to_string()),
                    ..Default::default()
                };
                // Route through the context-aware path so the declared effect
                // and the run's event sink are stamped onto every parallel call.
                let res = self.execute_with_context(&tc_clone, ctx, custom_timeout).await;
                ExecutedToolResult {
                    tool_call: tc_clone,
                    result: res,
                }
            }
        });

        join_all(futures).await
    }
}
