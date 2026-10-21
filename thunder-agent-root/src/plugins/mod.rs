#[cfg(feature = "conversation")]
pub mod conversation;
#[cfg(feature = "mcp")]
pub mod mcp;
#[cfg(feature = "conversation")]
pub mod memory;
pub mod metrics;
pub mod mode;
pub mod permission_rules;
#[cfg(feature = "skills")]
pub mod skills;
#[cfg(feature = "conversation")]
pub mod standard;
pub mod status;
pub mod web;

#[cfg(feature = "conversation")]
pub use conversation::ConversationPlugin;
#[cfg(feature = "mcp")]
pub use mcp::McpPlugin;
#[cfg(feature = "conversation")]
pub use memory::MemoryPlugin;
pub use metrics::{
    MetricsPlugin, MetricsSnapshot, SessionMetricsTracker, METRICS_UPDATE_EVENT_KIND,
};
pub use mode::{AllowRule, Mode, ModePlugin, MODE_CHANGED_EVENT_KIND};
#[cfg(feature = "skills")]
pub use skills::SkillsPlugin;
#[cfg(feature = "conversation")]
pub use standard::StandardHostBuilder;
pub use status::{AgentPhase, AgentStatusSnapshot, StatusPlugin, AGENT_STATUS_EVENT_KIND};
pub use web::WebPlugin;
