pub mod output;
pub mod permission_guard;
pub mod resource;
pub mod security;
pub mod telemetry;
pub mod transaction;

pub use output::OutputPostProcessorMiddleware;
pub use permission_guard::PermissionGuardMiddleware;
pub use resource::ResourceGuardMiddleware;
pub use security::SecurityGuardMiddleware;
pub use telemetry::SystemNotice;
pub use transaction::{TempFileGuard, TransactionMiddleware};

use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

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

    /// Assembles the complete standard 4-layer Onion Middleware Pipeline.
    ///
    /// Layer order:
    ///   1. SecurityGuardMiddleware (Path Jail, forbidden commands)
    ///   2. ResourceGuardMiddleware (ReadFile OOM defense, cancellation telemetry)
    ///   3. TransactionMiddleware (.arp/tmp atomic shadow write, permissions, cleanup)
    ///   4. OutputPostProcessorMiddleware (Scratchpad lossless persistence, truncation)
    ///   5. RegistryTerminalHandler (Core tool invocation)
    pub fn standard(
        workspace_root: std::path::PathBuf,
        extra_workspace_roots: &[std::path::PathBuf],
        registry: ToolRegistry,
        scratchpad: Option<crate::tools::scratchpad::ScratchpadManager>,
    ) -> Self {
        Self::configured(
            workspace_root,
            extra_workspace_roots,
            registry,
            scratchpad,
            &crate::types::config::MiddlewareConfig::default(),
            crate::types::config::Permission::default(),
            None,
            None,
        )
    }

    /// Assembles an Onion Middleware Pipeline adhering to caller's `MiddlewareConfig`.
    ///
    /// `policy` and `ui` are the judge's inputs. Passing `None` for either
    /// degrades safely: without a policy the guard has nothing to ask and lets
    /// everything through (the host's tool registration is then the only
    /// ceiling), and without a UI every `Ask` resolves as cancelled, so a
    /// headless host refuses rather than assuming consent.
    #[allow(clippy::too_many_arguments)]
    pub fn configured(
        workspace_root: std::path::PathBuf,
        extra_workspace_roots: &[std::path::PathBuf],
        mut registry: ToolRegistry,
        scratchpad: Option<crate::tools::scratchpad::ScratchpadManager>,
        cfg: &crate::types::config::MiddlewareConfig,
        permission: crate::types::config::Permission,
        policy: Option<Arc<crate::types::policy::SessionPolicy>>,
        ui: Option<Arc<dyn crate::types::ui::HostUi>>,
    ) -> Self {
        let max_output_bytes = 64 * 1024;
        let terminal: Arc<dyn ToolHandler> = if cfg.enable_output_post_processor {
            registry.clear_scratchpad();
            Arc::new(RegistryTerminalHandler::new(registry))
        } else {
            Arc::new(RegistryTerminalHandler::new(registry))
        };

        let mut pipeline = Self::new(terminal);
        // Outermost gate: a denied capability never reaches the workspace.
        let mut all_roots = vec![workspace_root.clone()];
        all_roots.extend(extra_workspace_roots.iter().cloned());
        let mut extra_roots_list: Vec<std::path::PathBuf> = extra_workspace_roots.to_vec();
        // Automatically whitelist the scratchpad directory so the model can inspect
        // persisted oversized logs without being blocked by the workspace path jail.
        if let Some(ref sp) = scratchpad {
            let sp_dir = sp.session_dir().to_path_buf();
            if !all_roots.contains(&sp_dir) {
                all_roots.push(sp_dir.clone());
            }
            if !extra_roots_list.contains(&sp_dir) {
                extra_roots_list.push(sp_dir);
            }
        }
        // One layer, outermost. It judges with the policy when given one, and
        // otherwise falls back to the bare tier so the pipeline is still
        // defended for embedders that have not adopted `SessionPolicy` yet.
        let mut guard = match policy {
            Some(policy) => permission_guard::PermissionGuardMiddleware::new(
                policy,
                ui.unwrap_or_else(|| {
                    Arc::new(crate::types::ui::NullHostUi) as Arc<dyn crate::types::ui::HostUi>
                }),
            ),
            None => permission_guard::PermissionGuardMiddleware::new_tier_only(
                permission,
                all_roots.clone(),
            ),
        };
        guard = guard.with_workspace_roots(all_roots);
        pipeline.add_middleware(Arc::new(guard));
        if cfg.enable_security_guard {
            pipeline.add_middleware(Arc::new(
                SecurityGuardMiddleware::new(&workspace_root).with_extra_roots(extra_roots_list),
            ));
        }
        if cfg.enable_resource_guard {
            pipeline.add_middleware(Arc::new(ResourceGuardMiddleware::new(&workspace_root)));
        }
        if cfg.enable_transaction {
            pipeline.add_middleware(Arc::new(TransactionMiddleware::new(&workspace_root)));
        }
        if cfg.enable_output_post_processor {
            pipeline.add_middleware(Arc::new(OutputPostProcessorMiddleware::new(
                max_output_bytes,
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
