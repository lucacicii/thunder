//! Thunder Suggest — Heuristic follow-up suggestion engine and prompt ghost text generator.
//!
//! Provides zero-latency heuristic follow-up questions, lightweight LLM suggestion generation,
//! and suggestion engines for Desktop (Daemon) and TUI.

pub mod engine;
pub mod heuristic;
pub mod llm_suggest;

pub use engine::SuggestionEngine;
pub use heuristic::generate_heuristic_suggestions;
pub use llm_suggest::generate_llm_suggestions;
