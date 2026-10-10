pub mod output;
pub mod permission_guard;
pub mod telemetry;

pub use output::OutputPostProcessorMiddleware;
pub use permission_guard::PermissionGuardMiddleware;
pub use telemetry::SystemNotice;

use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Default cap for a single tool result before it is truncated or spilled to
/// the scratchpad.
pub const DEFAULT_MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// An Onion Middleware wrapping around tool execution.
///
/// Middlewares execute in outer-to-inner order before invoking the tool,
/// and inner-to-outer order when processing the `ToolExecutionResult`.
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    /// Identifier or layer name (e.g., "SecurityGuard", "Transaction", "ProcessGuard")
    fn name(&self) -> &str;

    /// Execute the middleware logic, optionally forwarding to `next`.
    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult;
}

/// Handler interface capable of processing a `ToolCall`.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
    ) -> ToolExecutionResult;
}

/// Terminal handler that dispatches to the underlying `ToolRegistry`.
pub struct RegistryTerminalHandler {
    registry: ToolRegistry,
}

impl RegistryTerminalHandler {
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }
}

#[async_trait]
impl ToolHandler for RegistryTerminalHandler {
    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        self.registry
            // The caller's own context, so `caller` and `route` survive the hop.
            .execute_tool_call(call, ctx.clone(), timeout)
            .await
    }
}

struct MiddlewareRunner {
    middleware: Arc<dyn ToolMiddleware>,
    next: Arc<dyn ToolHandler>,
}

#[async_trait]
impl ToolHandler for MiddlewareRunner {
    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        self.middleware
            .handle(call, ctx, timeout, self.next.clone())
            .await
    }
}

/// Onion Middleware Pipeline coordinating tool calls through an ordered stack of middlewares.
#[derive(Clone)]
pub struct ToolPipeline {
    middlewares: Vec<Arc<dyn ToolMiddleware>>,
    terminal: Arc<dyn ToolHandler>,
}

impl ToolPipeline {
    pub fn new(terminal: Arc<dyn ToolHandler>) -> Self {
        Self {
            middlewares: Vec::new(),
            terminal,
        }
    }

    pub fn from_registry(registry: ToolRegistry) -> Self {
        Self::new(Arc::new(RegistryTerminalHandler::new(registry)))
    }

    /// The kernel's own pipeline: dispatch straight to the registry, with the
    /// output post-processor on top when a scratchpad is available.
    ///
    /// Nothing here knows what a workspace, a shell or a file is. Capability
    /// packs assemble their own stack (path jail, atomic writes, change
    /// detection) on top of [`ToolPipeline::from_registry`] and hand it to the
    /// loop through [`crate::loop_engine::engine::AgentLoop::with_pipeline_builder`].
    pub fn baseline(
        registry: ToolRegistry,
        scratchpad: Option<crate::tools::scratchpad::ScratchpadManager>,
    ) -> Self {
        let terminal: Arc<dyn ToolHandler> =
            Arc::new(RegistryTerminalHandler::new(registry.clone()));
        let mut pipeline = Self::new(terminal);
        if scratchpad.is_some() {
            pipeline.add_middleware(Arc::new(OutputPostProcessorMiddleware::new(
                DEFAULT_MAX_TOOL_OUTPUT_BYTES,
                scratchpad,
            )));
        }
        pipeline
    }

    pub fn with_middleware(mut self, mw: Arc<dyn ToolMiddleware>) -> Self {
        self.middlewares.push(mw);
        self
    }

    pub fn add_middleware(&mut self, mw: Arc<dyn ToolMiddleware>) {
        self.middlewares.push(mw);
    }

    /// Whether a middleware with this name is in the stack.
    pub fn has_middleware(&self, name: &str) -> bool {
        self.middlewares.iter().any(|m| m.name() == name)
    }

    pub fn middlewares(&self) -> &[Arc<dyn ToolMiddleware>] {
        &self.middlewares
    }

    /// Execute a tool call through the entire onion pipeline.
    pub async fn execute(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        let mut current = self.terminal.clone();
        for mw in self.middlewares.iter().rev() {
            current = Arc::new(MiddlewareRunner {
                middleware: mw.clone(),
                next: current,
            });
        }
        current.handle(call, ctx, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct TestMiddleware {
        id: usize,
        order: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl ToolMiddleware for TestMiddleware {
        fn name(&self) -> &str {
            "TestMiddleware"
        }

        async fn handle(
            &self,
            call: &ToolCall,
            ctx: &ToolExecutionContext,
            timeout: Option<Duration>,
            next: Arc<dyn ToolHandler>,
        ) -> ToolExecutionResult {
            self.order.lock().push(format!("enter_{}", self.id));
            let mut res = next.handle(call, ctx, timeout).await;
            self.order.lock().push(format!("exit_{}", self.id));
            res.output.push_str(&format!(" [M{}]", self.id));
            res
        }
    }

    struct MockTerminal;

    #[async_trait]
    impl ToolHandler for MockTerminal {
        async fn handle(
            &self,
            _call: &ToolCall,
            _ctx: &ToolExecutionContext,
            _timeout: Option<Duration>,
        ) -> ToolExecutionResult {
            ToolExecutionResult::success("core".to_string(), Duration::from_millis(5))
        }
    }

    #[tokio::test]
    async fn test_onion_execution_order() {
        let order = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let m1 = Arc::new(TestMiddleware {
            id: 1,
            order: order.clone(),
        });
        let m2 = Arc::new(TestMiddleware {
            id: 2,
            order: order.clone(),
        });

        let pipeline = ToolPipeline::new(Arc::new(MockTerminal))
            .with_middleware(m1)
            .with_middleware(m2);

        let call = ToolCall::new_function("call_1", "test", "{}");
        let ctx = ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
            ..Default::default()
        };

        let res = pipeline.execute(&call, &ctx, None).await;

        let sequence = order.lock().clone();
        assert_eq!(sequence, vec!["enter_1", "enter_2", "exit_2", "exit_1"]);
        assert_eq!(res.output, "core [M2] [M1]");
    }
}
