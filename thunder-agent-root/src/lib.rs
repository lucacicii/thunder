pub mod error;
pub mod host;
pub mod plugin;
pub mod plugins;
pub mod registry;
pub mod roles;
pub mod selector;

pub mod prelude {
    pub use crate::error::PluginError;
    pub use crate::host::{RootRunHandle, RootRunOptions, RootRunResult, ThunderRoot};
    pub use crate::plugin::{
        PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec,
    };
    #[cfg(feature = "mcp")]
    pub use crate::plugins::mcp::*;
    #[cfg(feature = "script-plugin")]
    pub use crate::plugins::script_plugin::*;
    #[cfg(feature = "skills")]
    pub use crate::plugins::skills::*;
    #[cfg(feature = "conversation")]
    pub use crate::plugins::standard::{baseline_forced_plugins, StandardHostBuilder};
    #[cfg(feature = "conversation")]
    pub use crate::plugins::ConversationPlugin;
    pub use crate::registry::{ActivePluginSet, PluginRegistry};
    pub use crate::roles::{Persona, RoleRegistry, RoleSpec};
    pub use crate::selector::{
        default_selection_cache, invalidate_all_session_selections, invalidate_session_selection,
        PluginSelection, PluginSelector, SelectionCache,
    };
}

pub use prelude::*;
