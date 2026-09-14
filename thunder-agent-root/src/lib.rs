//! # Thunder Root
//!
//! Extensible microkernel host and autonomous plugin orchestration engine for Thunder Agent.
//!
//! Built around `thunder-agent-loop` as the fundamental core unit, treating
//! conversation storage, multi-agent orchestration, skills parsers, and MCP tool discovery
//! as dynamically discoverable and registerable plugins.

pub mod error;
pub mod host;
pub mod plugin;
pub mod plugins;
pub mod registry;
pub mod selector;

pub mod prelude {
    pub use crate::error::PluginError;
    pub use crate::host::{RootRunHandle, RootRunOptions, RootRunResult, ThunderRoot};
    pub use crate::plugin::{
        PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec,
    };
    #[cfg(feature = "conversation")]
    pub use crate::plugins::ConversationPlugin;
    #[cfg(feature = "orchestra")]
    pub use crate::plugins::OrchestraPlugin;
    #[cfg(feature = "skills")]
    pub use crate::plugins::skills::*;
    #[cfg(feature = "mcp")]
    pub use crate::plugins::mcp::*;
    pub use crate::registry::{ActivePluginSet, PluginRegistry};
    pub use crate::selector::{PluginSelection, PluginSelector};
}

pub use prelude::*;
