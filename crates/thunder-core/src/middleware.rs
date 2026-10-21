use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use thunder_protocol::{ExecutionContext, ToolExecutionError};

/// Verdict returned by a middleware interceptor before tool execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MiddlewareVerdict {
    Allow,
    Reject { reason: String },
}

/// Abstract interceptor trait in the Safety & Middleware Pipeline (§1, §2.2).
///
/// The microkernel core defines this trait as an extension point for hosts
/// to inject authorization, approval dialogs, or rate limiting without
/// the core knowing any concrete tool business rules.
#[async_trait]
pub trait ToolMiddleware: Send + Sync {
    fn id(&self) -> &str;

    async fn before_call(
        &self,
        tool_name: &str,
        arguments: &Value,
        ctx: &ExecutionContext,
    ) -> MiddlewareVerdict;
}

/// Composable middleware pipeline executed before both direct and `thunder__call_tool` invocations.
#[derive(Clone, Default)]
pub struct MiddlewarePipeline {
    stages: Vec<Arc<dyn ToolMiddleware>>,
}

impl MiddlewarePipeline {
    /// Create a clean, empty middleware pipeline with no built-in business assumptions.
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    pub fn with_middleware(mut self, mw: Arc<dyn ToolMiddleware>) -> Self {
        self.stages.push(mw);
        self
    }

    pub async fn check(
        &self,
        tool_name: &str,
        arguments: &Value,
        ctx: &ExecutionContext,
    ) -> Result<(), ToolExecutionError> {
        for stage in &self.stages {
            if let MiddlewareVerdict::Reject { reason } =
                stage.before_call(tool_name, arguments, ctx).await
            {
                return Err(ToolExecutionError::NonZeroExit {
                    code: 403,
                    stderr: format!("[Middleware:{}] {reason}", stage.id()),
                });
            }
        }
        Ok(())
    }
}
