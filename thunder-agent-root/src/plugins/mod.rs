#[cfg(feature = "conversation")]
pub mod conversation;
#[cfg(feature = "skills")]
pub mod skills;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "script-plugin")]
pub mod script_plugin;

#[cfg(feature = "conversation")]
pub use conversation::ConversationPlugin;
#[cfg(feature = "skills")]
pub use skills::SkillsPlugin;
#[cfg(feature = "mcp")]
pub use mcp::McpPlugin;
#[cfg(feature = "script-plugin")]
pub use script_plugin::ScriptPlugin;
