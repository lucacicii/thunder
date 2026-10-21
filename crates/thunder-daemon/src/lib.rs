//! Thunder Daemon — NDJSON STDIO Sidecar for Desktop UIs (`agent-resume-panel`) and Headless Hosts.

pub mod protocol;
pub mod service;

pub use protocol::{
    normalize_mode, Attachment, DaemonRequest, DaemonResponse, ModeChangeSource, QuestionItem,
    QuestionOption, RenderCapabilities, PROTOCOL_VERSION,
};
pub use service::{DaemonService, DriverFactory};
