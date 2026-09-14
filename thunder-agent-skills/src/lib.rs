//! # Thunder Agent Skills
//!
//! Skill parser, directory discovery engine, and execution registry for Thunder Agent.
//! Supports `SKILL.md` frontmatter, JSON, YAML, and XML formats, autonomous prompt matching,
//! and dynamic `AgentTool` exposure.

pub mod error;
pub mod loader;
pub mod parser;
pub mod registry;
pub mod tools;
pub mod types;

pub mod prelude {
    pub use crate::error::SkillError;
    pub use crate::loader::SkillLoader;
    pub use crate::parser::SkillParser;
    pub use crate::registry::SkillRegistry;
    pub use crate::tools::{create_skill_tools, ListSkillsTool, LoadSkillTool, SearchSkillsTool};
    pub use crate::types::{Skill, SkillHandle, SkillSummary};
}

pub use prelude::*;
