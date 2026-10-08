//! Code capability pack for [`thunder_agent_loop`].
//!
//! The loop is a generic agent core: it streams the model, runs tools, prunes
//! context and stops. It deliberately does not know what a shell, a workspace
//! or a file is. Those belong here, in the pack that a *coding* host installs.
//!
//! Installing the pack gives an [`AgentLoop`](thunder_agent_loop::AgentLoop):
//!
//! * the built-in tools (`bash`, `read_file`, `write_file`, `grep`, `find`, `ls`);
//! * the code pipeline: path jail, OOM-resource guard, atomic shadow writes with
//!   process-wide per-path locking, and truncation/spill to the scratchpad;
//! * change detection — `write_file` and `bash` report what they touched as a
//!   `file_change` custom event, which is how a UI learns the workspace moved;
//! * the engineering default system prompt.
//!
//! A business host (a prototype editor, a form bot) installs a different pack —
//! or none at all — and none of this comes along for the ride.

pub mod builtin;
pub mod middleware;
pub mod pack;
pub mod pipeline;
pub mod prompt;

pub use middleware::file_change::FileChangeMiddleware;
pub use middleware::{ResourceGuardMiddleware, SecurityGuardMiddleware, TransactionMiddleware};
pub use pack::{install_code_pack, CodePackConfig};
pub use pipeline::{build_code_pipeline, CodePipelineConfig};
pub use prompt::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;
