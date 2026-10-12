#[cfg(feature = "conversation")]
pub mod conversation;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "conversation")]
pub mod memory;
pub mod mode;
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
pub use mode::{AllowRule, Mode, ModePlugin};
#[cfg(feature = "skills")]
pub use skills::SkillsPlugin;
#[cfg(feature = "conversation")]
pub use standard::{baseline_forced_plugins, StandardHostBuilder};
