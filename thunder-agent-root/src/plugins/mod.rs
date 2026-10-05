#[cfg(feature = "conversation")]
pub mod conversation;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "conversation")]
pub mod memory;
#[cfg(feature = "script-plugin")]
pub mod script_plugin;
#[cfg(feature = "skills")]
pub mod skills;
#[cfg(feature = "conversation")]
pub mod standard;

#[cfg(feature = "conversation")]
pub use conversation::ConversationPlugin;
#[cfg(feature = "mcp")]
pub use mcp::McpPlugin;
#[cfg(feature = "conversation")]
pub use memory::MemoryPlugin;
#[cfg(feature = "script-plugin")]
pub use script_plugin::ScriptPlugin;
#[cfg(feature = "skills")]
pub use skills::SkillsPlugin;
#[cfg(feature = "conversation")]
pub use standard::{baseline_forced_plugins, has_ts_plugins, StandardHostBuilder};
