//! Assembles the coding pipeline the kernel used to build for itself.

use crate::middleware::TransactionMiddleware;
use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_loop::tools::middleware::{PermissionGuardMiddleware, ToolPipeline};
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

/// Build the code stack around the loop's tool registry.
///
/// One layer, outermost first:
///   1. `PermissionGuardMiddleware` — human approval
///   2. `TransactionMiddleware`    — atomic shadow writes, per-path locking
///   3. `RegistryTerminalHandler`  — the actual tool
///
/// Three layers used to sit here and no longer do. `SecurityGuardMiddleware` was
/// the path jail: it answered "is this path inside the workspace" with a refusal,
/// and refused a great deal of ordinary work in the process. Its knowledge moved
/// to [`crate::scope::CodeScope`], where it classifies instead of judging, and
/// its one real rule — never run *this* command — moved to the kernel's guard,
/// which is where a rule no mode can override has to live.
/// `ResourceGuardMiddleware` and `OutputPostProcessorMiddleware` were both
/// second copies of checks made below them (in the read tool and in
/// `ToolRegistry::execute_tool_call`), and two ceilings for one behaviour is one
/// ceiling too many.
pub fn build_code_pipeline(inputs: PipelineContext) -> ToolPipeline {
    let cfg = CodePipelineConfig::from_agent_config(&inputs.config);
    let registry = inputs.registry.clone();

    let mut all_roots = vec![cfg.workspace_root.clone()];
    all_roots.extend(cfg.extra_workspace_roots.iter().cloned());
    // Spilled tool output lives outside the workspace; the approval dialog names
    // it as a granted root so the model is told the truth about where it may work.
    if let Some(ref sp) = inputs.scratchpad {
        let sp_dir = sp.session_dir().to_path_buf();
        if !all_roots.contains(&sp_dir) {
            all_roots.push(sp_dir);
        }
    }

    let terminal: Arc<dyn thunder_agent_loop::tools::middleware::ToolHandler> =
        Arc::new(thunder_agent_loop::tools::middleware::RegistryTerminalHandler::new(registry));
    let mut pipeline = ToolPipeline::new(terminal);

    // Add order is outermost-first (see `ToolPipeline::execute`).
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

    if cfg.middleware.enable_transaction {
        pipeline.add_middleware(Arc::new(TransactionMiddleware::new(&cfg.workspace_root)));
    }

    pipeline
}
