use crate::theme::MarkdownTheme;
use ratatui::text::{Line, Span};
use tui_markdown::{from_str_with_options, Options};

/// Fence info strings whose *contents* are themselves Markdown.
///
/// A model that answers "give me a Markdown sample" very often wraps the whole
/// document in a ```` ```markdown ```` fence. Rendering that as a wall of raw
/// code is technically correct and useless to read, so those fences are removed
/// before parsing and their contents are rendered as Markdown.
const MARKDOWN_FENCE_LANGS: [&str; 4] = ["markdown", "md", "mdx", "gfm"];

/// Render Markdown source into styled terminal lines via the `tui-markdown` engine.
///
/// Two pre-passes make LLM output render the way a reader expects:
///
/// * an unclosed fenced code block is synthetically closed, so streaming output
///   stays a code block instead of flashing into prose;
/// * a fence whose language is Markdown is unwrapped and its contents rendered
///   as Markdown rather than as literal source.
pub fn render_markdown(source: &str, width: usize, theme: &MarkdownTheme) -> Vec<Line<'static>> {
    let _ = width; // tui-markdown sizes tables from content; wrapping is the caller's job.
    let normalized = close_unterminated_fences(source);
    let flattened = unwrap_markdown_fences(&normalized);

    let options = Options::new(theme.clone());
    let text = from_str_with_options(&flattened, &options);

    // Detach from the borrowed input so the result is `'static` (the caller
    // stashes rendered lines in the timeline cache between frames).
    text.iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .iter()
                .map(|span| Span::styled(span.content.to_string(), span.style))
                .collect();
            let mut out = Line::from(spans);
            out.style = line.style;
            out
        })
        .collect()
}

/// Convenience function rendering with the default Thunder theme.
pub fn render_markdown_default(source: &str, width: usize) -> Vec<Line<'static>> {
    render_markdown(source, width, &MarkdownTheme::default())
}

/// An opening fence: its marker char, its length, and its info string.
struct Fence {
    marker: char,
    len: usize,
    info: String,
}

/// Parse a line that opens a fenced block, if it does.
fn opening_fence(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let marker = match trimmed.chars().next()? {
        '`' => '`',
        '~' => '~',
        _ => return None,
    };
    let len = trimmed.chars().take_while(|c| *c == marker).count();
    if len < 3 {
        return None;
    }
    // A backtick fence's info string may not contain a backtick (CommonMark).
    let info = trimmed[len..].trim();
    if marker == '`' && info.contains('`') {
        return None;
    }
    Some(Fence {
        marker,
        len,
        info: info.to_ascii_lowercase(),
    })
}

/// Whether `line` closes a fence opened with this marker and length.
fn closes_fence(line: &str, marker: char, len: usize) -> bool {
    let trimmed = line.trim();
    let count = trimmed.chars().take_while(|c| *c == marker).count();
    count >= len && trimmed.chars().all(|c| c == marker)
}

/// Append a synthetic closing fence for an unclosed ``` / ~~~ block.
///
/// While an LLM streams a reply, a code fence is frequently still open.
/// pulldown-cmark would swallow the rest of the message into a paragraph;
/// closing the fence keeps it a code block for the remainder of the stream.
fn close_unterminated_fences(source: &str) -> String {
    let mut open: Option<(char, usize)> = None;
    for line in source.lines() {
        match open {
            None => {
                if let Some(fence) = opening_fence(line) {
                    open = Some((fence.marker, fence.len));
                }
            }
            Some((marker, len)) => {
                if closes_fence(line, marker, len) {
                    open = None;
                }
            }
        }
    }

    match open {
        Some((marker, len)) => {
            let fence: String = std::iter::repeat(marker).take(len).collect();
            format!("{source}\n{fence}")
        }
        None => source.to_string(),
    }
}

/// Remove fences whose language is Markdown, keeping their contents.
///
/// This is what turns "the model wrapped the answer in ```` ```markdown ````"
/// from a wall of raw source into a rendered document. Fences of any other
/// language, and fences nested inside them, are untouched.
fn unwrap_markdown_fences(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut open: Option<(char, usize, bool)> = None; // marker, len, unwrap?

    for line in source.lines() {
        match open {
            None => {
                if let Some(fence) = opening_fence(line) {
                    let unwrap = MARKDOWN_FENCE_LANGS.iter().any(|lang| {
                        fence.info == *lang || fence.info.starts_with(&format!("{lang} "))
                    });
                    open = Some((fence.marker, fence.len, unwrap));
                    if !unwrap {
                        out.push_str(line);
                        out.push('\n');
                    }
                    continue;
                }
                out.push_str(line);
                out.push('\n');
            }
            Some((marker, len, unwrap)) => {
                if closes_fence(line, marker, len) {
                    open = None;
                    if !unwrap {
                        out.push_str(line);
                        out.push('\n');
                    }
                    continue;
                }
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_markdown_fences_and_keeps_other_fences() {
        let src = "intro\n\n````markdown\n# Title\n\n- a\n````\n\n```rust\nfn f() {}\n```\n";
        let out = unwrap_markdown_fences(src);
        assert!(out.contains("# Title"), "inner markdown must survive");
        assert!(
            !out.contains("````markdown"),
            "the markdown fence must be removed"
        );
        assert!(out.contains("```rust"), "a non-markdown fence must be kept");
    }

    #[test]
    fn four_backtick_fence_needs_a_four_backtick_close() {
        // A ``` inside a ```` block does not close it.
        let src = "````markdown\n```rust\nfn f() {}\n```\n````\n";
        let out = unwrap_markdown_fences(src);
        assert!(!out.contains("````"), "the outer fence is gone");
        assert!(out.contains("```rust"), "the nested fence stays intact");
    }
}
