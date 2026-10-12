//! Assembles the coding pipeline the kernel used to build for itself.

use std::path::PathBuf;
use thunder_agent_loop::tools::middleware::ToolPipeline;
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::PipelineContext;

/// Inputs the code pipeline needs beyond the loop's own state.
#[derive(Debug, Clone)]
pub struct CodePipelineConfig {
    pub workspace_root: PathBuf,
    pub extra_workspace_roots: Vec<PathBuf>,
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
        }
    }
}

/// Build the code stack around the loop's tool registry.
///
/// Which is to say: nothing wraps it. Four layers used to sit here and none
/// survive as a layer. `SecurityGuardMiddleware` was the path jail — its
/// knowledge moved to [`crate::scope::CodeScope`], which classifies instead of
/// judging, and its one real rule moved to the kernel's guard.
/// `ResourceGuardMiddleware` and `OutputPostProcessorMiddleware` were second
/// copies of checks made below them. `TransactionMiddleware` was a capability
/// wearing a layer's clothes, and now lives in
/// [`crate::builtin::fs::WriteFileTool`]. The permission layer dissolved into
/// the executor's funnel: the guard and the host's gate run there, in the one
/// method every call passes through, and can no longer be left out of an
/// assembly because there is no assembly.
///
/// The builder is kept rather than inlined so a host's call sites (and their
/// pipeline-builder hooks) keep working while the kernel finishes removing the
/// pipeline itself.
pub fn build_code_pipeline(inputs: PipelineContext) -> ToolPipeline {
    ToolPipeline::baseline(inputs.registry)
}
