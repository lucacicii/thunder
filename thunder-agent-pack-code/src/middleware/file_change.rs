//! Reports what a tool did to the workspace, as a generic custom event.
//!
//! The kernel used to detect this itself, by name-matching `write_file` and
//! `bash` and diffing `git status` around them. That was a coding concept
//! living inside a generic loop. Here it is a *pack* layer doing the same work
//! and emitting `AgentEvent::Custom { kind: "file_change" }` — the loop carries
//! the event without knowing what a file is.

use async_trait::async_trait;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::tools::middleware::{ToolHandler, ToolMiddleware};
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{ToolExecutionContext, ToolExecutionResult};

/// Emits a `file_change` custom event for the two tools that can move the
/// workspace: `write_file` (from its own arguments) and `bash` (from the git
/// status delta around the command).
#[derive(Clone)]
pub struct FileChangeMiddleware {
    workspace: PathBuf,
}

impl FileChangeMiddleware {
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
        }
    }
}

#[async_trait]
impl ToolMiddleware for FileChangeMiddleware {
    fn name(&self) -> &str {
        "FileChangeMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        // Only `bash` needs a snapshot: it is the tool whose file effects are
        // invisible from its arguments.
        let is_bash = call.function.name == "bash";
        let before = if is_bash {
            git_status_snapshot(&self.workspace).await
        } else {
            HashSet::new()
        };

        let result = next.handle(call, ctx, timeout).await;

        let Some(sink) = ctx.event_sink.as_ref() else {
            return result;
        };

        if call.function.name == "write_file" {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
            {
                if let Some(path) = parsed.get("path").and_then(|v| v.as_str()) {
                    let bytes = parsed
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|c| c.len());
                    let action = if result.is_error { "failed" } else { "written" };
                    sink.emit_custom(
                        "file_change",
                        serde_json::json!({
                            "path": path,
                            "action": action,
                            "bytes": bytes,
                            "tool": "write_file",
                        }),
                    );
                }
            }
        } else if is_bash {
            let after = git_status_snapshot(&self.workspace).await;
            for entry in &after {
                if before.contains(entry) {
                    continue;
                }
                let path = if entry.len() > 3 {
                    entry[3..].trim().to_string()
                } else {
                    entry.clone()
                };
                let action = if entry.starts_with("??") {
                    "created"
                } else if entry.starts_with('D') || entry.contains(" D") {
                    "deleted"
                } else {
                    "modified"
                };
                sink.emit_custom(
                    "file_change",
                    serde_json::json!({
                        "path": path,
                        "action": action,
                        "tool": "bash",
                    }),
                );
            }
        }

        result
    }
}

/// `git status --porcelain` entries, or an empty set when the workspace is not
/// a git checkout (or git is unavailable).
async fn git_status_snapshot(workspace: &Path) -> HashSet<String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| line.len() > 3)
            .map(str::to_string)
            .collect(),
        _ => HashSet::new(),
    }
}
