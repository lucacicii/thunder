use crate::core::utf8::{safe_slice_from, safe_slice_to};
use crate::tools::sanitizer::sanitize_tool_output;
use crate::tools::scratchpad::ScratchpadManager;
use crate::types::message::ToolCall;
use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext, ToolExecutionResult};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ToolRegistry {
    /// `BTreeMap` (NOT `HashMap`): tool definitions are serialized into every
    /// LLM request, and a stable, deterministic order keeps the request prefix
    /// byte-identical across turns — a hard prerequisite for provider-side
    /// prompt caching. A `HashMap` would randomize the order per instance.
    tools: BTreeMap<String, Arc<dyn AgentTool>>,
    max_output_bytes: usize,
    default_timeout: Duration,
    scratchpad: Option<ScratchpadManager>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new(64 * 1024, Duration::from_secs(30))
    }
}

impl ToolRegistry {
    pub fn new(max_output_bytes: usize, default_timeout: Duration) -> Self {
        Self {
            tools: BTreeMap::new(),
            max_output_bytes,
            default_timeout,
            scratchpad: None,
        }
    }

    pub fn with_scratchpad(mut self, scratchpad: ScratchpadManager) -> Self {
        self.scratchpad = Some(scratchpad);
        self
    }

    pub fn set_scratchpad(&mut self, scratchpad: ScratchpadManager) {
        self.scratchpad = Some(scratchpad);
    }

    pub fn clear_scratchpad(&mut self) {
        self.scratchpad = None;
    }

    pub fn scratchpad(&self) -> Option<&ScratchpadManager> {
        self.scratchpad.as_ref()
    }

    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        let name = tool.definition().function.name;
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn AgentTool>> {
        self.tools.get(name).cloned()
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn get_definitions(&self) -> Vec<ToolDefinition> {
        // BTreeMap iteration is name-ordered: stable across instances and runs.
        self.tools.values().map(|t| t.definition()).collect()
    }

    pub async fn execute_tool_call(
        &self,
        call: &ToolCall,
        turn: usize,
        cancellation_token: CancellationToken,
        custom_timeout: Option<Duration>,
    ) -> ToolExecutionResult {
        let start = Instant::now();
        let tool_name = &call.function.name;

        let tool = match self.tools.get(tool_name) {
            Some(t) => t.clone(),
            None => {
                let available = self.tools.keys().cloned().collect::<Vec<_>>().join(", ");
                return ToolExecutionResult::error(
                    format!(
                        "Error: Tool '{}' not found. Available tools: [{}]",
                        tool_name, available
                    ),
                    start.elapsed(),
                );
            }
        };

        let parsed_args: serde_json::Value = if call.function.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            match serde_json::from_str(&call.function.arguments) {
                Ok(val) => val,
                Err(err) => {
                    return ToolExecutionResult::error(
                        format!(
                            "Error: Failed to parse JSON arguments for '{}': {}. Raw arguments: {}",
                            tool_name, err, call.function.arguments
                        ),
                        start.elapsed(),
                    );
                }
            }
        };

        let timeout = custom_timeout.unwrap_or(self.default_timeout);
        let ctx = ToolExecutionContext {
            tool_call_id: call.id.clone(),
            turn,
            cancellation_token: cancellation_token.clone(),
        };

        let result = tokio::select! {
            _ = cancellation_token.cancelled() => {
                Err("Tool execution cancelled".to_string())
            }
            res = tokio::time::timeout(timeout, tool.execute(parsed_args, &ctx)) => {
                match res {
                    Ok(inner_res) => inner_res,
                    Err(_) => Err(format!("Tool '{}' timed out after {:?}", tool_name, timeout)),
                }
            }
        };

        let duration = start.elapsed();
        match result {
            Ok(output) => {
                let sanitized = sanitize_tool_output(output);
                let original_bytes = sanitized.len();

                // If ScratchpadManager is configured, persist oversized output losslessly
                if let Some(ref sp) = self.scratchpad {
                    if original_bytes > sp.threshold_bytes() {
                        match sp
                            .process_tool_output(tool_name, turn, sanitized.clone())
                            .await
                        {
                            Ok(persisted_handle) => {
                                return ToolExecutionResult {
                                    output: persisted_handle,
                                    is_error: false,
                                    truncated: true,
                                    original_bytes,
                                    duration_ms: duration.as_millis() as u64,
                                    telemetry: None,
                                };
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Failed to persist output to scratchpad; falling back to in-memory truncation");
                            }
                        }
                    }
                }

                self.format_and_truncate(sanitized, false, duration)
            }
            Err(err_msg) => {
                let sanitized = sanitize_tool_output(err_msg);
                ToolExecutionResult::error(sanitized, duration)
            }
        }
    }

    fn format_and_truncate(
        &self,
        output: String,
        is_error: bool,
        duration: Duration,
    ) -> ToolExecutionResult {
        let original_bytes = output.len();
        if original_bytes <= self.max_output_bytes {
            return ToolExecutionResult {
                output,
                is_error,
                truncated: false,
                original_bytes,
                duration_ms: duration.as_millis() as u64,
                telemetry: None,
            };
        }

        // UTF-8 safe head/tail slicing (prevents panics on multi-byte characters)
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
            telemetry: None,
        }
    }
}
