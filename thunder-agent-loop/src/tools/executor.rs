use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::ToolExecutionResult;
use futures_util::future::join_all;
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
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        Self { registry }
    }

    pub async fn execute_all(
        &self,
        tool_calls: &[ToolCall],
        turn: usize,
        cancellation_token: CancellationToken,
        custom_timeout: Option<Duration>,
    ) -> Vec<ExecutedToolResult> {
        if tool_calls.is_empty() {
            return Vec::new();
        }

        let futures = tool_calls.iter().map(|tc| {
            let registry = self.registry.clone();
            let tc_clone = tc.clone();
            let token = cancellation_token.clone();

            async move {
                let res = registry
                    .execute_tool_call(&tc_clone, turn, token, custom_timeout)
                    .await;
                ExecutedToolResult {
                    tool_call: tc_clone,
                    result: res,
                }
            }
        });

        join_all(futures).await
    }
}
