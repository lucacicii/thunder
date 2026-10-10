//! Assembles the coding pipeline the kernel used to build for itself.

use crate::middleware::{
    FileChangeMiddleware, ResourceGuardMiddleware, SecurityGuardMiddleware, TransactionMiddleware,
};
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::tools::middleware::{
    OutputPostProcessorMiddleware, PermissionGuardMiddleware, ToolPipeline,
};
use thunder_agent_loop::types::config::{AgentConfig, MiddlewareConfig, Permission};
use thunder_agent_loop::types::ui::{HostUi, NullHostUi};
use thunder_agent_loop::PipelineContext;

/// Inputs the code pipeline needs beyond the loop's own state.
#[derive(Debug, Clone)]
pub struct CodePipelineConfig {
    pub workspace_root: PathBuf,
    pub extra_workspace_roots: Vec<PathBuf>,
    pub middleware: MiddlewareConfig,
    pub permission: Permission,
}

impl CodePipelineConfig {
    /// Derive the pack's inputs from a run's [`AgentConfig`].
    ///
    /// The loop never reads these fields; they exist for the pack. A missing
    /// workspace falls back to the process working directory, which is what the
    /// pre-pack default did.
    pub fn from_agent_config(config: &AgentConfig) -> Self {
        Self {
            workspace_root: config
                .workspace_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default()),
            extra_workspace_roots: config.extra_workspace_roots.clone(),
            middleware: config.middleware.clone(),
            permission: config.permission,
        }
    }
}

/// Build the full code stack around the loop's tool registry.
///
/// Layer order, outermost first:
///   1. `PermissionGuardMiddleware` — capability ceiling + human approval
///   2. `SecurityGuardMiddleware`  — path jail, forbidden commands
///   3. `ResourceGuardMiddleware`  — OOM defense for large reads
///   4. `TransactionMiddleware`    — atomic shadow writes, per-path locking
///   5. `OutputPostProcessorMiddleware` — scratchpad spill + truncation
///   6. `FileChangeMiddleware`     — `file_change` custom event
///   7. `RegistryTerminalHandler`  — the actual tool
///
/// `FileChangeMiddleware` sits innermost on purpose: it reports what the tool
/// did, so it must see the real result and not a refusal from a layer above.
pub fn build_code_pipeline(inputs: PipelineContext) -> ToolPipeline {
    let cfg = CodePipelineConfig::from_agent_config(&inputs.config);
    let registry = inputs.registry.clone();

    let mut all_roots = vec![cfg.workspace_root.clone()];
    all_roots.extend(cfg.extra_workspace_roots.iter().cloned());
    let mut extra_roots = cfg.extra_workspace_roots.clone();
    // The scratchpad holds spilled tool output; the model must be able to read
    // it back, so it is part of the jail even though it is outside the workspace.
    if let Some(ref sp) = inputs.scratchpad {
        let sp_dir = sp.session_dir().to_path_buf();
        if !all_roots.contains(&sp_dir) {
            all_roots.push(sp_dir.clone());
        }
        if !extra_roots.contains(&sp_dir) {
            extra_roots.push(sp_dir);
        }
    }

    let terminal: Arc<dyn thunder_agent_loop::tools::middleware::ToolHandler> =
        Arc::new(thunder_agent_loop::tools::middleware::RegistryTerminalHandler::new(registry));
    let mut pipeline = ToolPipeline::new(terminal);

    // Add order is outermost-first (see `ToolPipeline::execute`).
    // Outermost gate: a denied capability never reaches the workspace.
    let guard = match inputs.policy.clone() {
        Some(policy) => PermissionGuardMiddleware::new(
            policy,
            inputs
                .ui
                .clone()
                .unwrap_or_else(|| Arc::new(NullHostUi) as Arc<dyn HostUi>),
        ),
        None => PermissionGuardMiddleware::new_tier_only(cfg.permission, all_roots.clone()),
    }
    .with_workspace_roots(all_roots);
    pipeline.add_middleware(Arc::new(guard));

    if cfg.middleware.enable_security_guard {
        pipeline.add_middleware(Arc::new(
            SecurityGuardMiddleware::new(&cfg.workspace_root).with_extra_roots(extra_roots),
        ));
    }
    if cfg.middleware.enable_resource_guard {
        pipeline.add_middleware(Arc::new(ResourceGuardMiddleware::new(&cfg.workspace_root)));
    }
    if cfg.middleware.enable_transaction {
        pipeline.add_middleware(Arc::new(TransactionMiddleware::new(&cfg.workspace_root)));
    }
    if cfg.middleware.enable_output_post_processor {
        pipeline.add_middleware(Arc::new(OutputPostProcessorMiddleware::new(
            thunder_agent_loop::tools::middleware::DEFAULT_MAX_TOOL_OUTPUT_BYTES,
            inputs.scratchpad.clone(),
        )));
    }

    // Innermost of our own layers: it must observe the tool's real outcome, so
    // it sits closest to the terminal handler.
    pipeline.add_middleware(Arc::new(FileChangeMiddleware::new(
        cfg.workspace_root.clone(),
    )));

    pipeline
}
