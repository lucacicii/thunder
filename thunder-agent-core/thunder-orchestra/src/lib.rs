//! Scheduler B: compose many [`thunder_agent_loop::AgentLoop`] units.
//!
//! B never drives A's turns. It only `start` / `join` / `cancel` complete units.

pub mod config;
pub mod delegate;
pub mod health;
pub mod mock;
pub mod router;
pub mod scheduler;
pub mod store;

pub use config::{OrchestraConfig, Topology, UnitSpec};
pub use delegate::DelegateTool;
pub use health::{HealthCheck, HealthReport, HealthStatus};
pub use router::{IntentRouter, RoutingDecision};
pub use scheduler::{ScheduledRun, Scheduler};
pub use store::RunStore;
