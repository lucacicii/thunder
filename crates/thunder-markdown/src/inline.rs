use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Style used for ordinary inline text.
fn text_style() -> Style {
    Style::default().fg(Color::Rgb(226, 232, 240))
}

/// Inline `code` spans: light cyan on a slate chip.
fn code_style() -> Style {
    Style::default()
        .fg(Color::Rgb(125, 211, 252))
        .bg(Color::Rgb(30, 41, 59))
}

/// Lightweight inline-only Markdown parser for one-line terminal strings
/// (used for the user's own prompt echo, which is not rendered through the
/// full block engine).
///
/// Handles `code`, **bold**, *italic*, ~~strike~~ and [link](url); anything it
/// cannot close degrades to plain text instead of swallowing the rest of the line.
pub fn parse_inline(source: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut buf = String::new();

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), text_style()));
            }
        };
    }

    while i < len {
        // Inline code
        if chars[i] == '`' {
            flush!();
            i += 1;
            let mut code = String::new();
            let mut closed = false;
            while i < len {
                if chars[i] == '`' {
                    closed = true;
                    i += 1;
                    break;
                }
                code.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(format!(" {code} "), code_style()));
            } else {
                spans.push(Span::styled(format!("`{code}"), code_style()));
            }
            continue;
        }

        // Bold
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let marker = chars[i];
            flush!();
            i += 2;
            let mut bold = String::new();
            let mut closed = false;
            while i + 1 < len {
                if chars[i] == marker && chars[i + 1] == marker {
                    closed = true;
                    i += 2;
                    break;
                }
                bold.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(
                    bold,
                    Style::default()
                        .fg(Color::Rgb(255, 255, 255))
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!("{marker}{marker}{bold}"),
                    text_style(),
                ));
            }
            continue;
        }

        // Strikethrough
        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            flush!();
            i += 2;
            let mut struck = String::new();
            let mut closed = false;
            while i + 1 < len {
                if chars[i] == '~' && chars[i + 1] == '~' {
                    closed = true;
                    i += 2;
                    break;
                }
                struck.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(
                    struck,
                    Style::default()
                        .fg(Color::Rgb(100, 116, 139))
                        .add_modifier(Modifier::CROSSED_OUT),
                ));
            } else {
                spans.push(Span::styled(format!("~~{struck}"), text_style()));
            }
            continue;
        }

        // Italic (intra-word underscores stay literal, per CommonMark)
        if chars[i] == '*' || (chars[i] == '_' && (i == 0 || !chars[i - 1].is_alphanumeric())) {
            let marker = chars[i];
            flush!();
            i += 1;
            let mut italic = String::new();
            let mut closed = false;
            while i < len {
                if chars[i] == marker
                    && (marker != '_' || i + 1 == len || !chars[i + 1].is_alphanumeric())
                {
                    closed = true;
                    i += 1;
                    break;
                }
                italic.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(
                    italic,
                    Style::default()
                        .fg(Color::Rgb(203, 213, 225))
                        .add_modifier(Modifier::ITALIC),
                ));
            } else {
                spans.push(Span::styled(format!("{marker}{italic}"), text_style()));
            }
            continue;
        }

        // Link
        if chars[i] == '[' {
            flush!();
            i += 1;
            let mut label = String::new();
            let mut closed_bracket = false;
            while i < len {
                if chars[i] == ']' {
                    closed_bracket = true;
                    i += 1;
                    break;
                }
                label.push(chars[i]);
                i += 1;
            }
            if closed_bracket && i < len && chars[i] == '(' {
                i += 1;
                let mut url = String::new();
                let mut closed_paren = false;
                while i < len {
                    if chars[i] == ')' {
                        closed_paren = true;
                        i += 1;
                        break;
                    }
                    url.push(chars[i]);
                    i += 1;
                }
                if closed_paren {
                    spans.push(Span::styled(
                        format!("{label} ↗"),
                        Style::default()
                            .fg(Color::Rgb(56, 189, 248))
                            .add_modifier(Modifier::UNDERLINED),
                    ));
                    continue;
                }
            }
            spans.push(Span::styled(format!("[{label}"), text_style()));
            continue;
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush!();
    spans
}
