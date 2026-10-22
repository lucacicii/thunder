use crate::block::{parse_blocks, CalloutKind, MarkdownBlock};
use crate::inline::parse_inline;
use crate::syntax::highlight_code_line;
use crate::theme::MarkdownTheme;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// Render Markdown source into an aesthetic, pre-wrapped `Vec<Line<'static>>` for Ratatui.
pub fn render_markdown(source: &str, width: usize, theme: &MarkdownTheme) -> Vec<Line<'static>> {
    let effective_width = width.max(20);
    let blocks = parse_blocks(source);
    let mut lines = Vec::new();

    for block in blocks {
        match block {
            MarkdownBlock::Heading { level, text } => {
                let (prefix, style) = match level {
                    1 => ("█ ", theme.h1),
                    2 => ("│ ", theme.h2),
                    3 => ("• ", theme.h3),
                    _ => ("  ", theme.h4_to_h6),
                };
                let mut heading_spans = vec![Span::styled(prefix, style)];
                heading_spans.extend(parse_inline(&text, theme));
                lines.push(Line::from(heading_spans));
                lines.push(Line::from(""));
            }
            MarkdownBlock::CodeFence {
                lang,
                lines: code_lines,
                is_closed,
            } => {
                let badge = if lang.is_empty() {
                    "code".to_string()
                } else {
                    lang.clone()
                };
                let content_width = effective_width.saturating_sub(4).max(10);

                // Top card header: ╭─ [lang] ───────╮
                let prefix = format!("╭─ [{badge}] ");
                let fill_len = content_width.saturating_sub(prefix.len()).max(2);
                let top_border = format!("{prefix}{}╮", "─".repeat(fill_len));
                lines.push(Line::from(vec![Span::styled(
                    top_border,
                    theme.code_border,
                )]));

                for (idx, code_line) in code_lines.iter().enumerate() {
                    let mut row_spans = Vec::new();
                    row_spans.push(Span::styled("│ ", theme.code_border));
                    let line_no = format!("{:>2} │ ", idx + 1);
                    row_spans.push(Span::styled(line_no, theme.code_line_number));

                    let highlighted = highlight_code_line(code_line, &lang, theme);
                    row_spans.extend(highlighted);
                    lines.push(Line::from(row_spans));
                }

                // Bottom card footer: ╰─────────────────╯
                let bottom_char = if is_closed { "╰" } else { "├" };
                let bottom_label = if is_closed { "" } else { " (streaming…) " };
                let fill_bottom = content_width.saturating_sub(bottom_label.len()).max(4);
                let bottom_border =
                    format!("{bottom_char}{}{bottom_label}╯", "─".repeat(fill_bottom));
                lines.push(Line::from(vec![Span::styled(
                    bottom_border,
                    theme.code_border,
                )]));
                lines.push(Line::from(""));
            }
            MarkdownBlock::Callout {
                kind,
                title,
                lines: body_lines,
            } => {
                let (icon, style) = match kind {
                    CalloutKind::Tip => ("💡 TIP: ", theme.callout_tip),
                    CalloutKind::Note => ("ℹ️ NOTE: ", theme.callout_note),
                    CalloutKind::Warning => ("⚠️ WARNING: ", theme.callout_warning),
                    CalloutKind::Important => ("🚨 IMPORTANT: ", theme.callout_important),
                };
                lines.push(Line::from(vec![
                    Span::styled("▎ ", theme.quote_border),
                    Span::styled(icon, style),
                    Span::styled(title, style),
                ]));
                for b_line in body_lines {
                    let mut b_spans = vec![Span::styled("▎   ", theme.quote_border)];
                    b_spans.extend(parse_inline(&b_line, theme));
                    lines.push(Line::from(b_spans));
                }
                lines.push(Line::from(""));
            }
            MarkdownBlock::Blockquote(quote_lines) => {
                for q_line in quote_lines {
                    let mut q_spans = vec![Span::styled("▎ ", theme.quote_border)];
                    q_spans.extend(parse_inline(&q_line, theme));
                    lines.push(Line::from(q_spans));
                }
                lines.push(Line::from(""));
            }
            MarkdownBlock::TaskItem { checked, text } => {
                let (icon, style) = if checked {
                    ("  ✔ ", theme.checkbox_checked)
                } else {
                    ("  ○ ", theme.checkbox_unchecked)
                };
                let mut spans = vec![Span::styled(icon, style)];
                spans.extend(parse_inline(&text, theme));
                lines.push(Line::from(spans));
            }
            MarkdownBlock::ListItem {
                indent,
                bullet,
                text,
            } => {
                let pad = " ".repeat(indent + 2);
                let mut spans = vec![
                    Span::raw(pad),
                    Span::styled(format!("{bullet} "), theme.list_bullet),
                ];
                spans.extend(parse_inline(&text, theme));
                lines.push(Line::from(spans));
            }
            MarkdownBlock::ThematicBreak => {
                let rule_len = effective_width.min(60);
                lines.push(Line::from(Span::styled("─".repeat(rule_len), theme.rule)));
                lines.push(Line::from(""));
            }
            MarkdownBlock::Table { headers, rows } => {
                if headers.is_empty() {
                    continue;
                }
                let col_count = headers.len();
                let mut col_widths = vec![4usize; col_count];

                for (idx, h) in headers.iter().enumerate() {
                    col_widths[idx] = col_widths[idx].max(UnicodeWidthStr::width(h.as_str()) + 2);
                }
                for row in &rows {
                    for (idx, cell) in row.iter().enumerate() {
                        if idx < col_count {
                            col_widths[idx] =
                                col_widths[idx].max(UnicodeWidthStr::width(cell.as_str()) + 2);
                        }
                    }
                }

                // Header row
                let mut header_spans = Vec::new();
                header_spans.push(Span::styled("│", theme.table_border));
                for (idx, h) in headers.iter().enumerate() {
                    let w = col_widths[idx];
                    header_spans.push(Span::styled(
                        format!(" {:<w$} ", h, w = w.saturating_sub(2)),
                        theme.table_header,
                    ));
                    header_spans.push(Span::styled("│", theme.table_border));
                }
                lines.push(Line::from(header_spans));

                // Separator
                let mut sep_spans = Vec::new();
                sep_spans.push(Span::styled("├", theme.table_border));
                for (idx, w) in col_widths.iter().enumerate() {
                    sep_spans.push(Span::styled("─".repeat(*w), theme.table_border));
                    if idx + 1 < col_count {
                        sep_spans.push(Span::styled("┼", theme.table_border));
                    }
                }
                sep_spans.push(Span::styled("┤", theme.table_border));
                lines.push(Line::from(sep_spans));

                // Rows
                for row in rows {
                    let mut row_spans = Vec::new();
                    row_spans.push(Span::styled("│", theme.table_border));
                    for (idx, w) in col_widths.iter().enumerate() {
                        let cell_val = row.get(idx).map(String::as_str).unwrap_or("");
                        row_spans.push(Span::styled(
                            format!(" {:<w$} ", cell_val, w = w.saturating_sub(2)),
                            theme.text,
                        ));
                        row_spans.push(Span::styled("│", theme.table_border));
                    }
                    lines.push(Line::from(row_spans));
                }
                lines.push(Line::from(""));
            }
            MarkdownBlock::Paragraph(para) => {
                let spans = parse_inline(&para, theme);
                lines.push(Line::from(spans));
                lines.push(Line::from(""));
            }
        }
    }

    lines
}

/// Convenience function rendering with the default theme.
pub fn render_markdown_default(source: &str, width: usize) -> Vec<Line<'static>> {
    let theme = MarkdownTheme::default();
    render_markdown(source, width, &theme)
}
