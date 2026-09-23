use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Security Guard Middleware.
///
/// Enforces workspace jail boundaries (preventing path traversal attacks like `../../etc/passwd`),
/// and blocks dangerous/destructive shell commands before execution.
#[derive(Clone)]
pub struct SecurityGuardMiddleware {
    workspace_root: PathBuf,
    forbidden_commands: Vec<String>,
}

impl SecurityGuardMiddleware {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let canonical_ws = ws.canonicalize().unwrap_or(ws);
        Self {
            workspace_root: canonical_ws,
            forbidden_commands: vec![
                "rm -rf /".to_string(),
                "rm -rf /*".to_string(),
                ":(){ :|:& };:".to_string(),
                "mkfs".to_string(),
                "dd if=".to_string(),
            ],
        }
    }

    pub fn with_forbidden_command(mut self, cmd: impl Into<String>) -> Self {
        self.forbidden_commands.push(cmd.into());
        self
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Normalizes and validates whether a target path is within the workspace jail.
    pub fn check_path(&self, target: &Path) -> Result<PathBuf, String> {
        let candidate = if target.is_relative() {
            self.workspace_root.join(target)
        } else {
            target.to_path_buf()
        };

        // Normalize path without requiring non-existent files to exist
        let normalized = normalize_path(&candidate);

        if !normalized.starts_with(&self.workspace_root) {
            return Err(format!(
                "Path traversal detected! Path '{}' escapes workspace root '{}'",
                target.display(),
                self.workspace_root.display()
            ));
        }

        Ok(normalized)
    }

    /// Checks if a bash command contains forbidden destructive patterns.
    pub fn check_command(&self, command: &str) -> Result<(), String> {
        let trimmed = command.trim();
        for forbidden in &self.forbidden_commands {
            if trimmed.contains(forbidden) {
                return Err(format!("Forbidden high-risk command pattern detected: '{}'", forbidden));
            }
        }
        Ok(())
    }
}

/// Normalizes a path by resolving `.` and `..` components logically.
fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(Component::Prefix(p)),
            Component::RootDir => out.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

#[async_trait]
impl ToolMiddleware for SecurityGuardMiddleware {
    fn name(&self) -> &str {
        "SecurityGuardMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();

        // 1. Inspect arguments for path traversal
        if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.function.arguments) {
            // Check 'path' parameter
            if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                if let Err(violation) = self.check_path(Path::new(path_str)) {
                    let notice = SystemNotice::new(
                        "SecurityGuard",
                        "Path traversal attack blocked",
                        "No file system modifications were performed. Command was halted at security perimeter.",
                    )
                    .with_guidance(format!(
                        "Confine all file operations within active workspace '{}'.",
                        self.workspace_root.display()
                    ));

                    return ToolExecutionResult::error(format!("Access Denied: {}", violation), start.elapsed())
                        .with_telemetry(notice);
                }
            }

            // Check 'cwd' parameter
            if let Some(cwd_str) = args.get("cwd").and_then(|v| v.as_str()) {
                if let Err(violation) = self.check_path(Path::new(cwd_str)) {
                    let notice = SystemNotice::new(
                        "SecurityGuard",
                        "Working directory traversal blocked",
                        "Process spawn aborted. Cwd escaped sandbox root.",
                    )
                    .with_guidance("Ensure working directory targets remain inside the workspace.");

                    return ToolExecutionResult::error(format!("Access Denied: {}", violation), start.elapsed())
                        .with_telemetry(notice);
                }
            }

            // Check bash commands for high-risk patterns
            if call.function.name == "bash" {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    if let Err(violation) = self.check_command(cmd) {
                        let notice = SystemNotice::new(
                            "SecurityGuard",
                            "Forbidden destructive command blocked",
                            "Command was intercepted before being dispatched to the shell subsystem.",
                        )
                        .with_guidance("Dangerous destructive shell operations are disabled by safety guardrails.");

                        return ToolExecutionResult::error(format!("Execution Blocked: {}", violation), start.elapsed())
                            .with_telemetry(notice);
                    }
                }
            }
        }

        // Passed security perimeter, forward to next layer
        next.handle(call, ctx, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct DummyNext;
    #[async_trait]
    impl ToolHandler for DummyNext {
        async fn handle(
            &self,
            _call: &ToolCall,
            _ctx: &ToolExecutionContext,
            _timeout: Option<Duration>,
        ) -> ToolExecutionResult {
            ToolExecutionResult::success("allowed".to_string(), Duration::from_millis(1))
        }
    }

    #[tokio::test]
    async fn test_path_traversal_blocked_with_telemetry() {
        let ws = std::env::temp_dir().join("thunder_sec_test");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let call = ToolCall::new_function(
            "call_sec_1",
            "write_file",
            serde_json::json!({
                "path": "../../etc/shadow",
                "content": "hacked"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_sec_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = guard.handle(&call, &ctx, None, Arc::new(DummyNext)).await;
        assert!(res.is_error);
        assert!(res.output.contains("Path traversal detected"));
        assert!(res.output.contains("[System Telemetry: SecurityGuard"));
        assert!(res.output.contains("Path traversal attack blocked"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_forbidden_command_blocked() {
        let ws = std::env::temp_dir().join("thunder_sec_test_cmd");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let call = ToolCall::new_function(
            "call_sec_2",
            "bash",
            serde_json::json!({
                "command": "sudo rm -rf /"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_sec_2".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = guard.handle(&call, &ctx, None, Arc::new(DummyNext)).await;
        assert!(res.is_error);
        assert!(res.output.contains("Forbidden high-risk command pattern"));
        assert!(res.output.contains("Dangerous destructive shell operations are disabled"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_valid_path_allowed() {
        let ws = std::env::temp_dir().join("thunder_sec_test_ok");
        let _ = std::fs::create_dir_all(&ws);
        let guard = SecurityGuardMiddleware::new(&ws);

        let call = ToolCall::new_function(
            "call_sec_3",
            "write_file",
            serde_json::json!({
                "path": "src/main.rs",
                "content": "fn main() {}"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_sec_3".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = guard.handle(&call, &ctx, None, Arc::new(DummyNext)).await;
        assert!(!res.is_error);
        assert_eq!(res.output, "allowed");

        let _ = std::fs::remove_dir_all(&ws);
    }
}
