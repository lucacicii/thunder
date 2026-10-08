//! One-call installation of the code capability pack onto a loop.

use crate::builtin::{
    BashTool, FindTool, GrepTool, ListDirTool, ReadFileTool, WriteFileTool,
};
use crate::pipeline::build_code_pipeline;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::engine::AgentLoop;

/// What to install. Defaults match the historical "coding agent" behaviour.
#[derive(Debug, Clone)]
pub struct CodePackConfig {
    /// Register `bash`, `read_file`, `write_file`, `grep`, `find`, `ls`.
    pub register_builtins: bool,
    /// Replace the run's system prompt with the engineering default when the
    /// host did not supply one of its own.
    pub apply_default_prompt: bool,
}

impl Default for CodePackConfig {
    fn default() -> Self {
        Self {
            register_builtins: true,
            apply_default_prompt: false,
        }
    }
}

/// Install the code pack onto a loop: its pipeline, and its built-in tools.
///
/// The pipeline builder is registered first so the built-ins registered after
/// it are picked up on the next rebuild, and every later `register_tool` keeps
/// running through the pack's layers.
pub fn install_code_pack(mut agent: AgentLoop, config: &CodePackConfig) -> AgentLoop {
    agent = agent.with_pipeline_builder(Arc::new(build_code_pipeline));

    if config.register_builtins {
        agent.register_tool(Arc::new(BashTool::default()));
        agent.register_tool(Arc::new(ReadFileTool::default()));
        agent.register_tool(Arc::new(WriteFileTool::default()));
        agent.register_tool(Arc::new(GrepTool::default()));
        agent.register_tool(Arc::new(FindTool::default()));
        agent.register_tool(Arc::new(ListDirTool::default()));
    }

    agent
}
