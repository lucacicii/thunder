use crate::theme::MarkdownTheme;
use ratatui::text::Span;

/// Parse inline formatting (bold, italic, strikethrough, inline code, links) into styled Spans.
pub fn parse_inline(text: &str, theme: &MarkdownTheme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut buf = String::new();

    macro_rules! flush_buf {
        () => {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), theme.text));
            }
        };
    }

    while i < len {
        // 1. Inline code: `...`
        if chars[i] == '`' {
            flush_buf!();
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
                spans.push(Span::styled(format!(" {code} "), theme.inline_code));
            } else {
                // Streaming lenient: unclosed code span renders with code style
                spans.push(Span::styled(format!(" `{code}"), theme.inline_code));
            }
            continue;
        }

        // 2. Bold: **...** or __...__
        if i + 1 < len
            && ((chars[i] == '*' && chars[i + 1] == '*')
                || (chars[i] == '_' && chars[i + 1] == '_'))
        {
            let marker = chars[i];
            flush_buf!();
            i += 2;
            let mut bold_text = String::new();
            let mut closed = false;
            while i + 1 < len {
                if chars[i] == marker && chars[i + 1] == marker {
                    closed = true;
                    i += 2;
                    break;
                }
                bold_text.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(bold_text, theme.bold));
            } else {
                spans.push(Span::styled(
                    format!("{marker}{marker}{bold_text}"),
                    theme.text,
                ));
            }
            continue;
        }

        // 3. Strikethrough: ~~...~~
        if i + 1 < len && chars[i] == '~' && chars[i + 1] == '~' {
            flush_buf!();
            i += 2;
            let mut strike_text = String::new();
            let mut closed = false;
            while i + 1 < len {
                if chars[i] == '~' && chars[i + 1] == '~' {
                    closed = true;
                    i += 2;
                    break;
                }
                strike_text.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(strike_text, theme.strikethrough));
            } else {
                spans.push(Span::styled(format!("~~{strike_text}"), theme.text));
            }
            continue;
        }

        // 4. Italic: *...* or _..._
        if chars[i] == '*' || chars[i] == '_' {
            let marker = chars[i];
            flush_buf!();
            i += 1;
            let mut italic_text = String::new();
            let mut closed = false;
            while i < len {
                if chars[i] == marker {
                    closed = true;
                    i += 1;
                    break;
                }
                italic_text.push(chars[i]);
                i += 1;
            }
            if closed {
                spans.push(Span::styled(italic_text, theme.italic));
            } else {
                spans.push(Span::styled(format!("{marker}{italic_text}"), theme.text));
            }
            continue;
        }

        // 5. Link: [text](url)
        if chars[i] == '[' {
            flush_buf!();
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
                    spans.push(Span::styled(format!("{label} ↗"), theme.link));
                    continue;
                }
            }
            spans.push(Span::styled(format!("[{label}"), theme.text));
            continue;
        }

        buf.push(chars[i]);
        i += 1;
    }

    flush_buf!();
    spans
}
