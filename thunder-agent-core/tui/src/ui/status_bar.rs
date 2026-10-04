use crate::app::{AgentStatus, App, FocusPane};
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Cell width of the prompt / continuation gutter (`❯ ` or the spinner).
const PROMPT_WIDTH: usize = 2;
/// The input box grows to this many rows and then scrolls internally, so a very
/// long prompt can never eat the transcript.
pub const MAX_INPUT_ROWS: usize = 6;

/// The staged-image badge, if any. Shared by the layout maths and the render, so
/// the two can never disagree about how wide it is.
fn attachment_badge(app: &App) -> Option<String> {
    (!app.pending_images.is_empty()).then(|| format!("🖼 {} ", app.pending_images.len()))
}

/// Cells consumed ahead of the first row's text by the attachment badge.
fn lead_width(app: &App) -> usize {
    attachment_badge(app)
        .map(|badge| UnicodeWidthStr::width(badge.as_str()))
        .unwrap_or(0)
}

/// The input split into display rows, as character-index ranges.
///
/// Row 0 is narrower than the rest because the working label and the badge share
/// it. An explicit newline starts a row; a too-long row soft-wraps.
fn input_row_ranges(app: &App, total_width: usize) -> Vec<(usize, usize)> {
    let first = total_width
        .saturating_sub(PROMPT_WIDTH + lead_width(app))
        .max(1);
    let rest = total_width.saturating_sub(PROMPT_WIDTH).max(1);

    let chars: Vec<char> = app.input.chars().collect();
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut width = first;

    for (i, ch) in chars.iter().enumerate() {
        if *ch == '\n' {
            rows.push((start, i));
            start = i + 1;
            used = 0;
            width = rest;
            continue;
        }
        let cell = UnicodeWidthChar::width(*ch).unwrap_or(0);
        // Never split a wide character across rows.
        if used + cell > width && used > 0 {
            rows.push((start, i));
            start = i;
            used = 0;
            width = rest;
        }
        used += cell;
    }
    rows.push((start, chars.len()));
    rows
}

/// How many rows the input box needs, capped at [`MAX_INPUT_ROWS`].
pub fn input_height(app: &App, total_width: usize) -> u16 {
    input_row_ranges(app, total_width)
        .len()
        .clamp(1, MAX_INPUT_ROWS) as u16
}

/// The caret as `(row, character offset within that row)`.
fn input_cursor_position(app: &App, total_width: usize) -> (usize, usize) {
    let ranges = input_row_ranges(app, total_width);
    let cursor = app.input_cursor.min(app.input.chars().count());
    for (row, (start, end)) in ranges.iter().enumerate() {
        if cursor <= *end {
            return (row, cursor - start);
        }
    }
    (ranges.len().saturating_sub(1), 0)
}

/// The prompt glyph. The busy spinner is *not* here: it lives on the separator
/// above the input box (`chat.rs`), so it never competes with what is being
/// typed.
fn prompt_span(app: &App, theme: &Theme) -> Span<'static> {
    match app.agent_status {
        AgentStatus::Idle
        | AgentStatus::Thinking
        | AgentStatus::Streaming
        | AgentStatus::ExecutingTool { .. } => Span::styled(
            "❯ ",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Done => Span::styled(
            "✔ ",
            Style::default()
                .fg(theme.assistant_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Error(_) => Span::styled(
            "✖ ",
            Style::default()
                .fg(theme.error_color)
                .add_modifier(Modifier::BOLD),
        ),
    }
}

pub fn render_input(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let show_cursor = app.focus == FocusPane::Input && app.agent_status == AgentStatus::Idle;

    let total_width = area.width.max(1) as usize;
    let rows = input_row_ranges(app, total_width);
    let (cursor_row, cursor_offset) = input_cursor_position(app, total_width);
    let visible = area.height.max(1) as usize;
    // Scroll the box itself so the caret is always on screen.
    let first_row = if cursor_row >= visible {
        cursor_row + 1 - visible
    } else {
        0
    };

    let chars: Vec<char> = app.input.chars().collect();
    let mut lines: Vec<Line> = Vec::new();

    for (row, (start, end)) in rows.iter().enumerate().skip(first_row).take(visible) {
        let row_chars = &chars[*start..*end];
        let mut spans: Vec<Span> = Vec::new();

        if row == 0 {
            spans.push(prompt_span(app, theme));
            if let Some(badge) = attachment_badge(app) {
                spans.push(Span::styled(
                    badge,
                    Style::default()
                        .fg(theme.tool_bubble)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        } else {
            // Continuation rows line up under the text, not under the prompt.
            spans.push(Span::raw(" ".repeat(PROMPT_WIDTH)));
        }

        if show_cursor && row == cursor_row {
            let offset = cursor_offset.min(row_chars.len());
            spans.push(Span::styled(
                row_chars[..offset].iter().collect::<String>(),
                theme.text_style(),
            ));
            match row_chars.get(offset).copied() {
                // The character under the caret is drawn in reverse video, so
                // nothing shifts as the caret moves.
                Some(c) => {
                    spans.push(Span::styled(
                        c.to_string(),
                        Style::default()
                            .fg(theme.bg)
                            .bg(theme.accent_primary)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::styled(
                        row_chars[offset + 1..].iter().collect::<String>(),
                        theme.text_style(),
                    ));
                }
                None => spans.push(Span::styled("█", Style::default().fg(theme.accent_primary))),
            }
        } else {
            spans.push(Span::styled(
                row_chars.iter().collect::<String>(),
                theme.text_style(),
            ));
        }

        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines).style(Style::default().bg(Color::Rgb(13, 17, 23)));
    f.render_widget(paragraph, area);
}

pub fn render_status_bar(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let mut hints = vec![
        Span::styled(
            " ? ",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("/help", Style::default().fg(theme.text_main)),
        Span::styled(" | ", theme.muted_style()),
        Span::styled("/resume", Style::default().fg(theme.assistant_bubble)),
        Span::styled(" | ", theme.muted_style()),
        Span::styled("/model", Style::default().fg(theme.text_main)),
        Span::styled(" | ", theme.muted_style()),
        Span::styled("/skills", Style::default().fg(theme.text_main)),
        if let Some(skill) = &app.active_skill {
            Span::styled(
                format!(" [{}]", skill.name),
                Style::default().fg(theme.tool_bubble),
            )
        } else {
            Span::raw("")
        },
        Span::styled(" | ", theme.muted_style()),
        Span::styled("/mcp", Style::default().fg(theme.text_main)),
        Span::styled(" | ", theme.muted_style()),
        Span::styled("/compact", Style::default().fg(theme.text_main)),
        Span::styled(" | ", theme.muted_style()),
        Span::styled(
            format!("Tokens: ~{}", app.conversation.stats.total_tokens),
            theme.muted_style(),
        ),
        Span::styled(" | ", theme.muted_style()),
        // Input waiting for the run to reach a turn boundary.
        if app.queued.total() > 0 {
            Span::styled(
                format!("📥 {} queued", app.queued.total()),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        if app.queued.total() > 0 {
            Span::styled(" | ", theme.muted_style())
        } else {
            Span::raw("")
        },
        if app.is_paused() {
            Span::styled("/unpause resume", Style::default().fg(theme.highlight))
        } else if app.is_running() {
            Span::styled(
                "/pause hold · Esc cancel",
                Style::default().fg(theme.text_muted),
            )
        } else {
            Span::styled("Ctrl+C / Esc", Style::default().fg(theme.text_muted))
        },
        if app.is_paused() {
            Span::styled(" ⏸", Style::default().fg(theme.error_color))
        } else {
            Span::raw("")
        },
    ];

    if let Some((msg, instant)) = &app.status_message {
        if instant.elapsed().as_secs() < 4 {
            hints = vec![
                Span::styled(" ⚡ ", Style::default().fg(theme.highlight)),
                Span::styled(
                    msg,
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
        }
    }

    let paragraph =
        Paragraph::new(Line::from(hints)).style(Style::default().bg(Color::Rgb(10, 14, 20)));

    f.render_widget(paragraph, area);
}
