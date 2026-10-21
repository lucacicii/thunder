pub mod assembly;
pub mod context;
pub mod error;
pub mod host;
pub mod instructions;
pub mod loader;
pub mod plugins;

pub mod thunder_config;
pub mod trace_collector;

pub mod prelude {
    pub use crate::context::{HostUiHandle, PluginContext};
    pub use crate::error::PluginError;
    pub use crate::host::{RootRunHandle, RootRunOptions, RootRunResult, ThunderRoot};
    pub use crate::instructions::{
        discover_context_files, find_context_file_in_dir, ContextInstructionFile,
    };
    pub use crate::loader::{LoadedPlugins, PluginLoader};
    #[cfg(feature = "mcp")]
    pub use crate::plugins::mcp::*;
    pub use crate::plugins::metrics::{
        MetricsPlugin, MetricsSnapshot, SessionMetricsTracker, METRICS_UPDATE_EVENT_KIND,
    };
    #[cfg(feature = "skills")]
    pub use crate::plugins::skills::*;
    #[cfg(feature = "conversation")]
    pub use crate::plugins::standard::StandardHostBuilder;
    pub use crate::plugins::status::{
        AgentPhase, AgentStatusSnapshot, StatusPlugin, AGENT_STATUS_EVENT_KIND,
    };
    pub use crate::plugins::web::WebPlugin;
    #[cfg(feature = "conversation")]
    pub use crate::plugins::{ConversationPlugin, MemoryPlugin};
    pub use crate::thunder_config::{AgentSection, MemorySection, PluginSection, ThunderConfig};
    pub use crate::trace_collector::{TraceCollector, MAX_EVENTS, MAX_TOOL_TRACES};
    /// What a host's transcript can draw; feeds the default prompt's
    /// presentation rules. Re-exported so a daemon or TUI need not depend on
    /// the code pack directly.
    pub use thunder_agent_pack_code::RenderCapabilities;
}

pub use prelude::*;
