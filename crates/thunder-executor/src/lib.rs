//! Thunder Executor — Process lifecycle supervisor, sandbox execution, flood protection, scratchpad, and MCP client pool.

pub mod ansi;
pub mod flood_guard;
pub mod mcp_client;
pub mod scratchpad;
pub mod script_runner;

pub use ansi::strip_ansi_codes;
pub use flood_guard::{
    drain_stream_with_flood_guard, safe_slice_from, safe_slice_to, utf8_safe_prefix,
    FloodGuardedOutput, DEFAULT_MAX_OUTPUT_BYTES,
};
pub use mcp_client::{McpServerConfig, McpToolProvider, StdioMcpClient, DEFAULT_MAX_FRAME_BYTES};
pub use scratchpad::{
    default_thunder_scratchpad_dir, Artifact, ArtifactManifest, ScratchpadConfig, ScratchpadManager,
};
pub use script_runner::{
    execute_isolated_script, execute_isolated_script_with_limit, execute_shell_command,
    ScriptEntry, ScriptToolProvider,
};
