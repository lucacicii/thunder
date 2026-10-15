//! One-call installation of the code capability pack onto a loop.

use crate::builtin::{BashTool, FindTool, GrepTool, ListDirTool, ReadFileTool, WriteFileTool};
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

/// Install the code pack onto a loop: its pipeline, its scope, and its built-in
/// tools.
///
/// The scope comes along with the tools because a question about a path and a
/// kernel that does not know where that path is would be answering two
/// different questions.
///
/// Each tool gets the workspace as its default directory, so a relative path in
/// a call means "relative to the workspace" — the meaning it had when the
/// transaction layer joined the workspace root, and the meaning a bare tool
/// would lose by resolving against the process working directory instead.
pub fn install_code_pack(mut agent: AgentLoop, config: &CodePackConfig) -> AgentLoop {
    let scope = crate::scope::CodeScope::from_agent_config(agent.config(), None);
    agent = agent.with_tool_scope(Arc::new(scope));

    if config.register_builtins {
        let ws = agent.config().workspace_dir.clone();
        let mut bash = BashTool::default();
        let mut read = ReadFileTool::default();
        let mut write = WriteFileTool::default();
        let mut grep = GrepTool::default();
        let mut find = FindTool::default();
        let mut ls = ListDirTool::default();
        if let Some(ref dir) = ws {
            bash = bash.with_default_cwd(dir.clone());
            read = read.with_default_cwd(dir.clone());
            write = write.with_default_cwd(dir.clone());
            grep = grep.with_default_cwd(dir.clone());
            find = find.with_default_cwd(dir.clone());
            ls = ls.with_default_cwd(dir.clone());
        }
        agent.register_tool(Arc::new(bash));
        agent.register_tool(Arc::new(read));
        agent.register_tool(Arc::new(write));
        agent.register_tool(Arc::new(grep));
        agent.register_tool(Arc::new(find));
        agent.register_tool(Arc::new(ls));
    }

    agent
}

/// Returns the 6 coding capability tools as MCP-aligned `ToolPlugin` trait objects.
///
/// Enables modular, Lego-style assembly into any `AgentLoop` via `agent.register_plugin(...)`.
pub fn code_plugins(
    workspace_dir: Option<std::path::PathBuf>,
) -> Vec<Arc<dyn thunder_agent_loop::types::plugin::ToolPlugin>> {
    let mut bash = BashTool::default();
    let mut read = ReadFileTool::default();
    let mut write = WriteFileTool::default();
    let mut grep = GrepTool::default();
    let mut find = FindTool::default();
    let mut ls = ListDirTool::default();
    if let Some(ref dir) = workspace_dir {
        bash = bash.with_default_cwd(dir.clone());
        read = read.with_default_cwd(dir.clone());
        write = write.with_default_cwd(dir.clone());
        grep = grep.with_default_cwd(dir.clone());
        find = find.with_default_cwd(dir.clone());
        ls = ls.with_default_cwd(dir.clone());
    }
    vec![
        Arc::new(bash),
        Arc::new(read),
        Arc::new(write),
        Arc::new(grep),
        Arc::new(find),
        Arc::new(ls),
    ]
}
