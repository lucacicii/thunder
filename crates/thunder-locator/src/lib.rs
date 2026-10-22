//! Thunder Locator — Autonomous Workspace Directory Locator and Hybrid Memory Registry.
//!
//! Provides zero-intrusion automatic workspace directory routing based on user prompts,
//! explicit project profiles (~/.thunder/workspaces.json), and implicit episodic
//! and historical interaction memory from thunder-memory (~/.thunder/memory.db).

pub mod locator;
pub mod memory_bridge;
pub mod model;
pub mod registry;

pub use locator::{extract_tokens, WorkspaceLocator};
pub use memory_bridge::MemoryBridge;
pub use model::{CandidateSource, LocateVerdict, WorkspaceCandidate, WorkspaceRecord};
pub use registry::WorkspaceRegistry;
