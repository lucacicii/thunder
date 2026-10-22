use crate::theme::MarkdownTheme;
use ratatui::text::{Line, Span};
use tui_markdown::{from_str_with_options, Options};

/// Render Markdown source into styled terminal lines via the `tui-markdown` engine.
///
/// A streaming pre-pass makes partially-received LLM output safe to render:
/// an unclosed fenced code block is synthetically closed so the parser keeps
/// rendering it as a code block instead of flashing it as prose.
pub fn render_markdown(source: &str, width: usize, theme: &MarkdownTheme) -> Vec<Line<'static>> {
    let _ = width; // tui-markdown sizes tables from content; wrapping is the caller's job.
    let normalized = close_unterminated_fences(source);

    let options = Options::new(theme.clone());
    let text = from_str_with_options(&normalized, &options);

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

/// Append a synthetic closing fence for every unclosed ``` / ~~~ block.
///
/// While an LLM streams a reply, a code fence is frequently still open.
/// pulldown-cmark would swallow the rest of the message into a paragraph;
/// closing the fence keeps it a code block for the remainder of the stream.
fn close_unterminated_fences(source: &str) -> String {
    let mut open_fence: Option<char> = None;
    for line in source.lines() {
        let trimmed = line.trim_start();
        match open_fence {
            None => {
                if let Some(fence) = fence_char(trimmed) {
                    open_fence = Some(fence);
                }
            }
            Some(fence) => {
                if is_closing_fence(trimmed, fence) {
                    open_fence = None;
                }
            }
        }
    }

    match open_fence {
        Some('`') => format!("{source}\n```"),
        Some('~') => format!("{source}\n~~~"),
        _ => source.to_string(),
    }
}

fn fence_char(line: &str) -> Option<char> {
    if line.starts_with("```") && !line.trim_start_matches('`').trim().is_empty() {
        Some('`')
    } else if line.starts_with("```") {
        // A bare ``` opener without a language is still a fence opener.
        Some('`')
    } else if line.starts_with("~~~") {
        Some('~')
    } else {
        None
    }
}

fn is_closing_fence(line: &str, fence: char) -> bool {
    let trimmed = line.trim();
    match fence {
        '`' => trimmed.starts_with("```") && trimmed.chars().all(|c| c == '`'),
        '~' => trimmed.starts_with("~~~") && trimmed.chars().all(|c| c == '~'),
        _ => false,
    }
}
