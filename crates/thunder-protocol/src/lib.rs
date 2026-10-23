//! Thunder Protocol — Unified Tool Schema, RPC contracts, Context Envelope, and Event Bus.

pub mod event;
pub mod mcp;
pub mod message;
pub mod paths;
pub mod schema;
pub mod stream;

pub use event::{
    AgentEvent, AgentStats, EventSink, FinishReason, ObservedEvent, ThunderEvent,
    ToolExecResultPayload,
};
pub use mcp::{
    ContextEnvelopeMeta, JsonRpcError, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    McpCallToolResult, McpContentBlock, McpListToolsResult, McpToolCallParams, McpToolInfo,
    JSONRPC_VERSION,
};
pub use message::{
    ChatMessage, ContentPart, FunctionDefinition, PromptCacheWarmSettings, Role, ToolCall,
    ToolCallFunction, ToolDefinition, TurnStats,
};
pub use paths::{
    real_home_dir, real_home_dir_from, thunder_config_dir, thunder_config_dir_from, thunder_subdir,
    THUNDER_CONFIG_DIR_ENV, THUNDER_DIR_NAME,
};
pub use schema::{
    sanitize_identifier, ExecutionContext, ProgressKind, ToolDescriptor, ToolExecutionError,
    ToolExecutionResult, ToolNamespace, ToolProvider,
};
pub use stream::{
    ChatRequestOptions, LLMClientTrait, LLMStreamChunk, StreamToolCallDelta, UnconfiguredLLMClient,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn namespace_uri_and_priority_follow_spec() {
        let core = ToolNamespace::parse_uri("core://read_file").unwrap();
        let mcp = ToolNamespace::parse_uri("mcp://git/commit").unwrap();
        let script = ToolNamespace::parse_uri("script://git_squash_feature").unwrap();

        assert_eq!(core.exposed_name(), "core__read_file");
        assert_eq!(mcp.exposed_name(), "mcp__git__commit");
        assert_eq!(script.exposed_name(), "script__git_squash_feature");

        assert!(script.priority() > mcp.priority());
        assert!(mcp.priority() > core.priority());
        assert_eq!(core.logical_name(), "read_file");
        assert_eq!(mcp.logical_name(), "commit");
        assert_eq!(script.logical_name(), "git_squash_feature");
    }

    #[test]
    fn context_envelope_serializes_with_underscore_meta() {
        let ctx = ExecutionContext::new(
            "sess-6f3b",
            PathBuf::from("/workspace/project"),
            Duration::from_secs(30),
        );
        let params = McpToolCallParams {
            name: "read_file".to_string(),
            arguments: json!({ "path": "src/lib.rs" }),
            meta: Some(ContextEnvelopeMeta::from(&ctx)),
        };

        let val = serde_json::to_value(&params).unwrap();
        assert_eq!(val["name"], "read_file");
        assert_eq!(val["arguments"]["path"], "src/lib.rs");
        assert_eq!(val["_meta"]["session_id"], "sess-6f3b");
        assert_eq!(val["_meta"]["workspace_cwd"], "/workspace/project");
        assert_eq!(val["_meta"]["timeout_ms"], 30000);
        // Absent, not empty: a server that predates the field must not see one.
        assert!(val["_meta"].get("workspace_dirs").is_none());
    }

    #[test]
    fn the_context_envelope_carries_additional_workspace_dirs() {
        let ctx = ExecutionContext::new(
            "sess-6f3b",
            PathBuf::from("/workspace/project"),
            Duration::from_secs(30),
        )
        .with_extra_workspace_dirs([PathBuf::from("/workspace/shared")]);

        assert_eq!(
            ctx.search_roots(),
            vec![
                PathBuf::from("/workspace/project"),
                PathBuf::from("/workspace/shared")
            ]
        );

        let meta = ContextEnvelopeMeta::from(&ctx);
        let val = serde_json::to_value(meta).unwrap();
        assert_eq!(val["workspace_dirs"][0], "/workspace/shared");
    }

    #[test]
    fn the_primary_root_is_never_listed_twice() {
        let ctx = ExecutionContext::new(
            "sess-6f3b",
            PathBuf::from("/workspace/project"),
            Duration::from_secs(30),
        )
        .with_extra_workspace_dirs([
            PathBuf::from("/workspace/project"),
            PathBuf::from("/workspace/shared"),
        ]);
        assert_eq!(ctx.search_roots().len(), 2);
    }

    #[test]
    fn error_normalization_distinguishes_tool_error_and_system_fault() {
        let ok = ToolExecutionResult::normalize(Ok("hello".to_string()));
        assert_eq!(
            ok,
            ToolExecutionResult::Success {
                output: "hello".to_string()
            }
        );

        let tool_err = ToolExecutionResult::normalize(Err(ToolExecutionError::NonZeroExit {
            code: 2,
            stderr: "missing file".to_string(),
        }));
        assert_eq!(
            tool_err,
            ToolExecutionResult::ToolError {
                code: 2,
                stderr: "missing file".to_string(),
                is_retryable: true,
            }
        );

        let timeout_fault = ToolExecutionResult::normalize(Err(ToolExecutionError::Timeout));
        assert!(matches!(
            timeout_fault,
            ToolExecutionResult::SystemFault { .. }
        ));
    }
}
