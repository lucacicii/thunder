//! # Thunder TUI
//!
//! Interactive Terminal User Interface for Thunder Agent.

pub mod app;
pub mod commands;
pub mod event;
pub mod mock;
pub mod picker;
pub mod runner;
pub mod ui;

pub mod prelude {
    pub use crate::app::{ActiveToolCall, AgentStatus, App, FocusPane, ViewMode};
    pub use crate::event::{AppEvent, EventHandler};
    pub use crate::runner::TuiRunner;
    pub use crate::ui::theme::Theme;
}

pub use prelude::*;
