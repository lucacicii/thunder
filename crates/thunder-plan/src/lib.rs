//! Thunder Plan — Plan Mode Governance Crate.
//!
//! Provides `write_plan` and `exit_plan_mode` tools via `PlanToolProvider`,
//! read-only shell AST verification via `read_only`, and `PlanModeMiddleware`.

pub mod middleware;
pub mod provider;
pub mod read_only;
pub mod tools;

pub use middleware::PlanModeMiddleware;
pub use provider::PlanToolProvider;
pub use read_only::{call_is_read_only, is_read_only};
pub use tools::{handle_exit_plan_mode, handle_write_plan, plan_file_path, ModeFlipCallback};
