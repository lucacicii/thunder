//! Shared helper for the pack's integration tests.
//!
//! Mirrors the old kernel `ToolPipeline::configured` signature so the tests read
//! the same, but routes assembly through the pack — which is where the code
//! layers now live.

use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::tools::middleware::ToolPipeline;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::tools::scratchpad::ScratchpadManager;
use thunder_agent_loop::types::config::{AgentConfig, MiddlewareConfig, Permission};
use thunder_agent_loop::types::policy::SessionPolicy;
use thunder_agent_loop::types::ui::HostUi;
use thunder_agent_loop::PipelineContext;

/// Assemble the code pipeline the way a coding host does.
#[allow(clippy::too_many_arguments)]
pub fn code_pipeline(
    workspace_root: PathBuf,
    extra_workspace_roots: &[PathBuf],
    registry: ToolRegistry,
    scratchpad: Option<ScratchpadManager>,
    middleware: &MiddlewareConfig,
    permission: Permission,
    policy: Option<Arc<SessionPolicy>>,
    ui: Option<Arc<dyn HostUi>>,
) -> ToolPipeline {
    let mut config = AgentConfig::new("test-model");
    config.workspace_dir = Some(workspace_root);
    config.extra_workspace_roots = extra_workspace_roots.to_vec();
    config.middleware = middleware.clone();
    config.permission = permission;
    thunder_agent_pack_code::build_code_pipeline(PipelineContext {
        config,
        registry,
        scratchpad,
        policy,
        ui,
    })
}
