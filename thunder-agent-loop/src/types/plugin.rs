use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::types::policy::ToolEffect;
use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};

/// Standard MCP Content item (aligned with Anthropic MCP 2024-11-05).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mime_type: String },
    #[serde(rename = "resource")]
    Resource { uri: String, text: Option<String> },
}

/// Standard MCP CallToolResult (aligned with Anthropic MCP 2024-11-05).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl CallToolResult {
    /// Create a success result with text content.
    pub fn success_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: Some(false),
        }
    }

    /// Create an error result with text content.
    pub fn error_text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolContent::Text { text: text.into() }],
            is_error: Some(true),
        }
    }

    /// Extract concatenated plain text from all content blocks.
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for item in &self.content {
            match item {
                ToolContent::Text { text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
                ToolContent::Image { mime_type, .. } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&format!("[Image: {mime_type}]"));
                }
                ToolContent::Resource { uri, text } => {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    if let Some(t) = text {
                        out.push_str(&format!("[Resource {uri}]: {t}"));
                    } else {
                        out.push_str(&format!("[Resource: {uri}]"));
                    }
                }
            }
        }
        out
    }

    pub fn is_err(&self) -> bool {
        self.is_error.unwrap_or(false)
    }
}

/// Base Plugin metadata and identity contract.
pub trait Plugin: Send + Sync {
    /// Unique identifier of the plugin (e.g., "read_file", "security_guard").
    fn id(&self) -> &str;

    /// Human-friendly name. Defaults to `id()`.
    fn name(&self) -> &str {
        self.id()
    }

    /// Semantic version of the plugin. Defaults to "0.1.0".
    fn version(&self) -> &str {
        "0.1.0"
    }

    /// Short description of what this plugin provides.
    fn description(&self) -> &str {
        ""
    }
}

/// Role A: Capability Plugin (ToolPlugin)
///
/// An extensible tool callable by the model or other tools.
/// Input and output are fully aligned with the MCP specification.
#[async_trait]
pub trait ToolPlugin: Plugin {
    /// Tool definition formatted for the model (name, description, inputSchema).
    fn definition(&self) -> ToolDefinition;

    /// Declared effect of this tool (Read, Write, Shell, etc.).
    fn effect(&self) -> ToolEffect {
        ToolEffect::of(&self.definition().function.name)
    }

    /// Execute the tool with MCP-aligned parameters and result.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String>;
}

/// Verdict returned by an InterceptorPlugin before execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreExecutionVerdict {
    /// Proceed with execution.
    Allow,
    /// Deny execution immediately with a reason given to the caller.
    Deny { reason: String },
    /// Require user approval before execution (Human-in-the-Loop).
    AskUser { prompt: String },
}

/// Well-known execution priorities for [`InterceptorPlugin`].
pub mod priority {
    /// Critical security guardrails (e.g. `rm -rf /`, dangerous commands). Always runs first.
    pub const CRITICAL_GUARD: i32 = 1000;
    /// Security policies and path sandboxing (e.g. `Scope`).
    pub const SECURITY_POLICY: i32 = 800;
    /// Standard execution modes and human-in-the-loop approvals (e.g. `Mode` plan/ask/yolo).
    pub const GOVERNANCE_MODE: i32 = 500;
    /// Normal default priority for general-purpose interceptors.
    pub const NORMAL: i32 = 500;
    /// Observability, telemetry, and structured tracing (runs outermost in onion).
    pub const OBSERVABILITY: i32 = 100;
}

/// Role B: Governance Interceptor Plugin (InterceptorPlugin)
///
/// Wraps around tool execution in the kernel pipeline.
/// Provides security guardrails, policy modes, auditing, and telemetry.
#[async_trait]
pub trait InterceptorPlugin: Plugin {
    /// Execution priority. Higher values execute earlier in `pre_execute`,
    /// and later in `post_execute` (Onion / Middleware pattern).
    /// Defaults to [`priority::NORMAL`] (500).
    fn priority(&self) -> i32 {
        priority::NORMAL
    }

    /// Hook executed before a tool call runs.
    async fn pre_execute(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<PreExecutionVerdict, String> {
        let _ = (tool_name, args, ctx);
        Ok(PreExecutionVerdict::Allow)
    }

    /// Hook executed after a tool call completes.
    async fn post_execute(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        result: &mut CallToolResult,
        ctx: &ToolExecutionContext,
    ) {
        let _ = (tool_name, args, result, ctx);
    }
}

/// Sub-tool invocation interface used by `ToolExecutionContext::invoke`.
#[async_trait]
pub trait SubToolInvoker: Send + Sync + std::fmt::Debug {
    async fn invoke_sub_tool(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String>;
}

/// Bridge adapter: Wraps an `Arc<dyn ToolPlugin>` as a legacy `AgentTool`.
///
/// Allows new `ToolPlugin` implementations to be directly registered into
/// existing `ToolRegistry` and `ToolExecutor` without breaking changes.
pub struct ToolPluginAdapter {
    plugin: Arc<dyn ToolPlugin>,
}

impl ToolPluginAdapter {
    pub fn new(plugin: Arc<dyn ToolPlugin>) -> Self {
        Self { plugin }
    }

    pub fn plugin(&self) -> &Arc<dyn ToolPlugin> {
        &self.plugin
    }
}

#[async_trait]
impl AgentTool for ToolPluginAdapter {
    fn definition(&self) -> ToolDefinition {
        self.plugin.definition()
    }

    fn effect(&self) -> ToolEffect {
        self.plugin.effect()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let res = self.plugin.execute(args, ctx).await?;
        if res.is_err() {
            Err(res.plain_text())
        } else {
            Ok(res.plain_text())
        }
    }
}
