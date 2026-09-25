//! Scheduler B: compose many [`thunder_agent_loop::AgentLoop`] units.
//!
//! B never drives A's turns. It only `start` / `join` / `cancel` complete units.

pub mod config;
pub mod decomposer;
pub mod delegate;
pub mod health;
pub mod mock;
pub mod router;
pub mod scheduler;
pub mod store;
pub mod synthesizer;

pub use config::{OrchestraConfig, Topology, UnitSpec};
pub use decomposer::{HeuristicDecomposer, SubTask, TaskDecomposer};
pub use delegate::DelegateTool;
pub use health::{HealthCheck, HealthReport, HealthStatus};
pub use router::{IntentRouter, RoutingDecision};
pub use scheduler::{ScheduledRun, Scheduler};
pub use store::RunStore;
pub use synthesizer::Synthesizer;
