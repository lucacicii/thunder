//! Thunder Markdown — Aesthetic, streaming-lenient, width-exact Markdown renderer for Ratatui and terminal UIs.
//!
//! Provides syntax-highlighted code cards, callout alerts, CJK-aligned tables, and inline bold/italic/code styling.

pub mod block;
pub mod inline;
pub mod render;
pub mod syntax;
pub mod theme;

pub use block::{parse_blocks, CalloutKind, MarkdownBlock};
pub use inline::parse_inline;
pub use render::{render_markdown, render_markdown_default};
pub use syntax::highlight_code_line;
pub use theme::MarkdownTheme;
