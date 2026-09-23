use crate::core::utf8::{safe_slice_from, safe_slice_to};
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::tools::sanitizer::sanitize_tool_output;
use crate::tools::scratchpad::ScratchpadManager;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// Output Post-Processor Middleware.
///
/// Handles output sanitization (stripping ANSI escapes, binary detection),
/// lossless persistence of oversized outputs to Scratchpad, and UTF-8 safe truncation.
#[derive(Clone)]
pub struct OutputPostProcessorMiddleware {
    max_output_bytes: usize,
    scratchpad: Option<ScratchpadManager>,
}

impl OutputPostProcessorMiddleware {
    pub fn new(max_output_bytes: usize, scratchpad: Option<ScratchpadManager>) -> Self {
        Self {
            max_output_bytes,
            scratchpad,
        }
    }

    pub fn with_scratchpad(mut self, scratchpad: ScratchpadManager) -> Self {
        self.scratchpad = Some(scratchpad);
        self
    }

    fn format_and_truncate(&self, output: String, is_error: bool, duration: Duration) -> ToolExecutionResult {
        let original_bytes = output.len();
        if original_bytes <= self.max_output_bytes {
            return ToolExecutionResult {
                output,
                is_error,
                truncated: false,
                original_bytes,
                duration_ms: duration.as_millis() as u64,
            };
        }

        // UTF-8 safe head/tail slicing
        let keep_side = self.max_output_bytes * 4 / 10;
        let head = safe_slice_to(&output, keep_side);
        let tail = safe_slice_from(&output, output.len().saturating_sub(keep_side));
        let omitted = original_bytes - head.len() - tail.len();

        let truncated_str = format!(
            "{}\n\n[... Output truncated: {} bytes omitted (total: {} bytes) ...]\n\n{}",
            head, omitted, original_bytes, tail
        );

        ToolExecutionResult {
            output: truncated_str,
            is_error,
            truncated: true,
            original_bytes,
            duration_ms: duration.as_millis() as u64,
        }
    }
}

#[async_trait]
impl ToolMiddleware for OutputPostProcessorMiddleware {
    fn name(&self) -> &str {
        "OutputPostProcessorMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        let res = next.handle(call, ctx, timeout).await;

        let sanitized = sanitize_tool_output(res.output);
        let duration = Duration::from_millis(res.duration_ms);

        if res.is_error {
            return ToolExecutionResult::error(sanitized, duration);
        }

        let original_bytes = sanitized.len();

        // 1. Lossless Scratchpad persistence if configured and oversized
        if let Some(ref sp) = self.scratchpad {
            if original_bytes > sp.threshold_bytes() && !sanitized.contains("[Large Output Saved to Disk]") {
                match sp.process_tool_output(&call.function.name, ctx.turn, sanitized.clone()).await {
                    Ok(persisted_handle) => {
                        let result = ToolExecutionResult {
                            output: persisted_handle,
                            is_error: false,
                            truncated: true,
                            original_bytes,
                            duration_ms: duration.as_millis() as u64,
                        };
                        return result;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to persist output to scratchpad; falling back to truncation");
                    }
                }
            }
        }

        // 2. Format and truncate within byte ceiling
        self.format_and_truncate(sanitized, false, duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct EchoHandler {
        text: String,
    }

    #[async_trait]
    impl ToolHandler for EchoHandler {
        async fn handle(
            &self,
            _call: &ToolCall,
            _ctx: &ToolExecutionContext,
            _timeout: Option<Duration>,
        ) -> ToolExecutionResult {
            ToolExecutionResult::success(self.text.clone(), Duration::from_millis(5))
        }
    }

    #[tokio::test]
    async fn test_output_middleware_truncation() {
        let long_output = "X".repeat(1000);
        let handler = Arc::new(EchoHandler { text: long_output });
        let middleware = OutputPostProcessorMiddleware::new(100, None);

        let call = ToolCall::new_function("call_out_1", "test", "{}");
        let ctx = ToolExecutionContext {
            tool_call_id: "call_out_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let res = middleware.handle(&call, &ctx, None, handler).await;
        assert!(res.truncated);
        assert!(res.output.contains("Output truncated"));
        assert_eq!(res.original_bytes, 1000);
    }
}
