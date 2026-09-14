#[cfg(feature = "conversation")]
pub mod conversation;
#[cfg(feature = "orchestra")]
pub mod orchestra;
#[cfg(feature = "skills")]
pub mod skills;
#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "conversation")]
pub use conversation::ConversationPlugin;
#[cfg(feature = "orchestra")]
pub use orchestra::OrchestraPlugin;
#[cfg(feature = "skills")]
pub use skills::SkillsPlugin;
#[cfg(feature = "mcp")]
pub use mcp::McpPlugin;
