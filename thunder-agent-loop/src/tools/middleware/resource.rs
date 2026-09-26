use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Resource Guard Middleware.
///
/// Defends against Memory Exhaustion (OOM) caused by unconstrained file reading,
/// and enforces standardized cancellation handling with omniscient telemetry reports.
#[derive(Clone)]
pub struct ResourceGuardMiddleware {
    workspace_root: PathBuf,
    max_read_bytes: u64,
}

impl ResourceGuardMiddleware {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            max_read_bytes: 10 * 1024 * 1024, // 10 MB default ceiling
        }
    }

    pub fn with_max_read_bytes(mut self, bytes: u64) -> Self {
        self.max_read_bytes = bytes;
        self
    }

    pub fn max_read_bytes(&self) -> u64 {
        self.max_read_bytes
    }
}

#[async_trait]
impl ToolMiddleware for ResourceGuardMiddleware {
    fn name(&self) -> &str {
        "ResourceGuardMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let start = Instant::now();

        // 1. Guard against Reading Oversized Files (OOM protection)
        if call.function.name == "read_file" {
            if let Ok(args) = serde_json::from_str::<serde_json::Value>(&call.function.arguments) {
                if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                    let limit = args.get("limit").and_then(|v| v.as_u64());
                    let p = Path::new(path_str);
                    let target_path = if p.is_relative() {
                        self.workspace_root.join(p)
                    } else {
                        p.to_path_buf()
                    };

                    if let Ok(meta) = tokio::fs::metadata(&target_path).await {
                        let file_size = meta.len();
                        // If file exceeds maximum allowed bytes and caller did not specify a page limit
                        if file_size > self.max_read_bytes && limit.is_none() {
                            let size_mb = file_size as f64 / (1024.0 * 1024.0);
                            let max_mb = self.max_read_bytes as f64 / (1024.0 * 1024.0);

                            let notice = SystemNotice::new(
                                "ResourceGuard",
                                "Oversized file read intercepted for memory protection",
                                format!(
                                    "Target file '{}' is {:.2} MB, exceeding the memory protection ceiling of {:.2} MB.",
                                    target_path.display(),
                                    size_mb,
                                    max_mb
                                ),
                            )
                            .with_guidance(
                                "To safely inspect large files without OOM, invoke 'read_file' with 'offset' and 'limit' (e.g. limit: 200).",
                            );

                            return ToolExecutionResult::error(
                                format!(
                                    "Error: File '{}' ({:.2} MB) exceeds maximum allowed single read limit of {:.2} MB.",
                                    target_path.display(),
                                    size_mb,
                                    max_mb
                                ),
                                start.elapsed(),
                            )
                            .with_telemetry(notice);
                        }
                    }
                }
            }
        }

        // 2. Pre-execution cancellation check
        if ctx.cancellation_token.is_cancelled() {
            let notice = SystemNotice::new(
                "ResourceGuard",
                "Tool execution cancelled before launch",
                "Invocation halted immediately by user cancellation signal.",
            )
            .with_guidance("You may safely formulate a new prompt or retry.");

            return ToolExecutionResult::error(
                "Tool execution cancelled by user signal".to_string(),
                start.elapsed(),
            )
            .with_telemetry(notice);
        }

        // Forward to inner layers
        let res = next.handle(call, ctx, timeout).await;

        // If the execution failed due to cancellation, augment with telemetry notice
        if res.is_error && res.output.contains("Tool execution cancelled") {
            let notice = SystemNotice::new(
                "ResourceGuard",
                "Tool execution interrupted by cancellation signal",
                "Running process/operation was halted cleanly upon receiving user signal.",
            )
            .with_guidance(
                "The cancelled operation has been stopped without persistent corruption.",
            );

            return res.with_telemetry(notice);
        }

        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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
            ToolExecutionResult::success("ok".to_string(), Duration::from_millis(1))
        }
    }

    #[tokio::test]
    async fn test_oom_guard_blocks_huge_file() {
        let ws = std::env::temp_dir().join(format!("thunder_res_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&ws);
        let huge_file = ws.join("huge.log");

        // Create a 2MB file and set limit to 1MB
        {
            let mut f = std::fs::File::create(&huge_file).expect("create");
            let chunk = vec![b'A'; 1024 * 1024]; // 1MB
            f.write_all(&chunk).expect("write 1");
            f.write_all(&chunk).expect("write 2");
        }

        let guard = ResourceGuardMiddleware::new(&ws).with_max_read_bytes(1024 * 1024); // 1MB limit

        let call = ToolCall::new_function(
            "call_oom_1",
            "read_file",
            serde_json::json!({
                "path": "huge.log"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_oom_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = guard.handle(&call, &ctx, None, Arc::new(DummyNext)).await;
        assert!(res.is_error);
        assert!(res
            .output
            .contains("exceeds maximum allowed single read limit"));
        assert!(res.output.contains("[System Telemetry: ResourceGuard"));
        assert!(res.output.contains("Oversized file read intercepted"));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_oom_guard_permits_chunked_read() {
        let ws =
            std::env::temp_dir().join(format!("thunder_res_test_chunked_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&ws);
        let huge_file = ws.join("huge.log");

        {
            let mut f = std::fs::File::create(&huge_file).expect("create");
            let chunk = vec![b'B'; 1024 * 1024];
            f.write_all(&chunk).expect("write");
            f.write_all(&chunk).expect("write");
        }

        let guard = ResourceGuardMiddleware::new(&ws).with_max_read_bytes(1024 * 1024);

        // Caller specifies limit: 50 -> permitted!
        let call = ToolCall::new_function(
            "call_oom_2",
            "read_file",
            serde_json::json!({
                "path": "huge.log",
                "offset": 1,
                "limit": 50
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_oom_2".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = guard.handle(&call, &ctx, None, Arc::new(DummyNext)).await;
        assert!(!res.is_error);
        assert_eq!(res.output, "ok");

        let _ = std::fs::remove_dir_all(&ws);
    }
}
