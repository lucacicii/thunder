//! Thunder Markdown — Aesthetic, streaming-lenient Markdown renderer for Ratatui,
//! powered by the `tui-markdown` engine (pulldown-cmark + GFM alerts, tables,
//! footnotes, and syntax-highlighted fenced code).
//!
//! Thunder adds one thing the stock engine lacks: a streaming pre-pass that
//! synthetically closes unclosed code fences so partially-received LLM output
//! renders as a code block instead of flashing between prose and code.

pub mod inline;
pub mod render;
pub mod theme;

pub use inline::parse_inline;
pub use render::{render_markdown, render_markdown_default};
pub use theme::MarkdownTheme;
