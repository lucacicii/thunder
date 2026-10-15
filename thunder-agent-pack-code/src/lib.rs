//! Code capability pack for [`thunder_agent_loop`].
//!
//! The loop is a generic agent core: it streams the model, runs tools, prunes
//! context and stops. It deliberately does not know what a shell, a workspace
//! or a file is. Those belong here, in the pack that a *coding* host installs.
//!
//! Installing the pack gives an [`AgentLoop`](thunder_agent_loop::AgentLoop):
//!
//! * the built-in tools (`bash`, `read_file`, `write_file`, `grep`, `find`, `ls`);
//! * truncation/spill to the scratchpad — which the registry does for every
//!   call, pack or no pack, so the pack does not wrap the registry at all;
//! * [`CodeScope`] — the host's answer to where a call lands, so the policy half
//!   can tell an in-workspace write from a reaching-out one;
//! * atomic writes with process-wide per-path locking — a property of
//!   [`builtin::fs::WriteFileTool`] itself, not a layer that could be left out
//!   of an assembly;
//! * change detection — `write_file` and `bash` report what they touched as a
//!   `file_change` custom event, which is how a UI learns the workspace moved;
//! * the engineering default system prompt.
//!
//! A business host (a prototype editor, a form bot) installs a different pack —
//! or none at all — and none of this comes along for the ride.

pub mod builtin;
pub mod file_change;
pub mod pack;
pub mod prompt;
pub mod scope;

pub use file_change::FILE_CHANGE;
pub use pack::{code_plugins, install_code_pack, CodePackConfig};
pub use prompt::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;
pub use scope::CodeScope;
