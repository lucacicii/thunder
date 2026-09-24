use crate::tools::middleware::{ToolMiddleware, ToolPipeline};
use crate::tools::registry::ToolRegistry;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use futures_util::future::join_all;
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
}

impl ToolExecutor {
    pub fn new(registry: ToolRegistry) -> Self {
        let pipeline = ToolPipeline::from_registry(registry.clone());
        Self { registry, pipeline }
    }

    pub fn with_standard_pipeline(
        registry: ToolRegistry,
        workspace_root: std::path::PathBuf,
        scratchpad: Option<crate::tools::scratchpad::ScratchpadManager>,
    ) -> Self {
        let pipeline = ToolPipeline::standard(workspace_root, registry.clone(), scratchpad);
        Self { registry, pipeline }
    }

    pub fn with_configured_pipeline(
        registry: ToolRegistry,
        workspace_root: std::path::PathBuf,
        scratchpad: Option<crate::tools::scratchpad::ScratchpadManager>,
        cfg: &crate::types::config::MiddlewareConfig,
        permission: crate::types::config::Permission,
    ) -> Self {
        let pipeline =
            ToolPipeline::configured(workspace_root, registry.clone(), scratchpad, cfg, permission);
        Self { registry, pipeline }
    }

    pub fn with_pipeline(registry: ToolRegistry, pipeline: ToolPipeline) -> Self {
        Self { registry, pipeline }
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
            let pipeline = self.pipeline.clone();
            let tc_clone = tc.clone();
            let token = cancellation_token.clone();

            async move {
                let ctx = ToolExecutionContext {
                    tool_call_id: tc_clone.id.clone(),
                    turn,
                    cancellation_token: token,
                };
                let res = pipeline.execute(&tc_clone, &ctx, custom_timeout).await;
                ExecutedToolResult {
                    tool_call: tc_clone,
                    result: res,
                }
            }
        });

        join_all(futures).await
    }
}
