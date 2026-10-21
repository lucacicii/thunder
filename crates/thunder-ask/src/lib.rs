//! Thunder Ask — Human-in-the-loop interactive question tool (`ask_user_question`) for Thunder Agent.
//!
//! Provides the `core__ask_user_question` tool provider, structured question parsers,
//! and abstract `UserQuestionHandler` implementations (channel-based for Desktop/TUI, mock for tests).

pub mod handler;
pub mod protocol;
pub mod provider;

pub use handler::{
    ChannelQuestionHandler, MockQuestionHandler, QuestionOutcome, QuestionPrompt,
    UserQuestionHandler,
};
pub use protocol::{parse_questions, QuestionItem, QuestionOption};
pub use provider::AskUserToolProvider;
