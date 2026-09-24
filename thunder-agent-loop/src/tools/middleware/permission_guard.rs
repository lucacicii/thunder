use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::config::Permission;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Permission Guard Middleware.
///
/// Second line of defense behind the host's tool-registration gate. The host
/// decides which built-in tools exist at all; this layer independently refuses
/// any privileged call that reaches execution anyway — e.g. a tool re-added by a
/// custom host path, or a nested call issued from another tool.
///
/// Rejection is reported with structured telemetry so the model learns the
/// ground truth ("the file was not written") instead of assuming success.
#[derive(Clone)]
pub struct PermissionGuardMiddleware {
    permission: Permission,
    /// Allowed workspace roots (primary first), surfaced in rejections so the
    /// model knows its legal targets instead of probing with other tools.
    workspace_roots: Vec<PathBuf>,
}

impl PermissionGuardMiddleware {
    pub fn new(permission: Permission) -> Self {
        Self {
            permission,
            workspace_roots: Vec::new(),
        }
    }

    /// Attach the jail's allowed roots for rejection guidance.
    pub fn with_workspace_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.workspace_roots = roots;
        self
    }

    pub fn permission(&self) -> Permission {
        self.permission
    }

    fn roots_display(&self) -> String {
        self.workspace_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[async_trait]
impl ToolMiddleware for PermissionGuardMiddleware {
    fn name(&self) -> &str {
        "PermissionGuardMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();
        let tool = call.function.name.as_str();

        if !self.permission.allows_builtin(tool) {
            let capability = match tool {
                "write_file" => "filesystem write",
                "bash" => "shell execution",
                _ => "the requested capability",
            };

            let roots_hint = if self.workspace_roots.is_empty() {
                String::new()
            } else {
                format!(
                    " Reads inside the allowed workspace roots are still available: [{}].",
                    self.roots_display()
                )
            };

            let notice = SystemNotice::new(
                "PermissionGuard",
                format!("Blocked '{}' — not granted by the active role", tool),
                format!(
                    "The call was rejected before execution. No {} occurred and the workspace is untouched.",
                    capability
                ),
            )
            .with_guidance(format!(
                "This role is read-only. Produce a plan or a patch description for the user to approve \
                 instead of attempting to modify the workspace directly, and do not try to work around \
                 this with other tools (e.g. shell writes).{}",
                roots_hint
            ));

            return ToolExecutionResult::error(
                format!("Error: tool '{}' is not available in the current role", tool),
                start.elapsed(),
            )
            .with_telemetry(notice);
        }

        next.handle(call, ctx, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::middleware::RegistryTerminalHandler;
    use crate::types::tool::{AgentTool, ToolDefinition};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    struct NoopTool(&'static str);

    #[async_trait]
    impl AgentTool for NoopTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition::new_function(self.0, "noop", json!({"type": "object"}))
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
            _ctx: &ToolExecutionContext,
        ) -> Result<String, String> {
            Ok("executed".to_string())
        }
    }

    fn registry_with(names: &[&'static str]) -> crate::tools::registry::ToolRegistry {
        let mut reg = crate::tools::registry::ToolRegistry::default();
        for n in names {
            reg.register(Arc::new(NoopTool(n)));
        }
        reg
    }

    fn call(name: &str) -> ToolCall {
        ToolCall::new_function("call_1", name, "{}")
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        }
    }

    async fn run(permission: Permission, tool: &str) -> ToolExecutionResult {
        let registry = registry_with(&["read_file", "write_file", "bash"]);
        let terminal: Arc<dyn ToolHandler> = Arc::new(RegistryTerminalHandler::new(registry));
        let pipeline = crate::tools::middleware::ToolPipeline::new(terminal)
            .with_middleware(Arc::new(PermissionGuardMiddleware::new(permission)));
        pipeline.execute(&call(tool), &ctx(), None).await
    }

    #[tokio::test]
    async fn read_role_allows_read_file() {
        let res = run(Permission::Read, "read_file").await;
        assert!(!res.is_error, "read_file must pass under Read");
        assert_eq!(res.output, "executed");
    }

    #[tokio::test]
    async fn read_role_blocks_write_file_and_bash() {
        for tool in ["write_file", "bash"] {
            let res = run(Permission::Read, tool).await;
            assert!(res.is_error, "{tool} must be blocked under Read");
            let notice = res.telemetry.expect("rejection carries telemetry");
            assert_eq!(notice.layer, "PermissionGuard");
        }
    }

    #[tokio::test]
    async fn write_role_allows_write_file_but_blocks_bash() {
        assert!(!run(Permission::Write, "write_file").await.is_error);
        assert!(run(Permission::Write, "bash").await.is_error);
    }

    #[tokio::test]
    async fn bash_role_allows_everything() {
        for tool in ["read_file", "write_file", "bash"] {
            assert!(!run(Permission::Bash, tool).await.is_error, "{tool} under Bash");
        }
    }

    #[tokio::test]
    async fn unknown_tool_is_not_gated_by_the_ladder() {
        // Plugin / MCP tools are gated elsewhere; the ladder must stay neutral.
        assert!(Permission::Read.allows_builtin("mcp_some_tool"));
        assert!(Permission::Read.allows_builtin("ts_plugin_tool"));
    }
}
