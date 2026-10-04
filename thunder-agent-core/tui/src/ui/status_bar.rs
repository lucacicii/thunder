use crate::app::{AgentStatus, App, FocusPane};
use crate::ui::theme::Theme;
use ratatui::layout::{Margin, Rect};
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
/// Blank rows above and below the text, so the editor reads as a panel.
pub const INPUT_VPAD: u16 = 1;

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

/// Height of the whole input box: its text rows plus the vertical padding.
///
/// `available_height` is what the caller can spare — the box never grows past
/// it, so a short terminal keeps its transcript instead of being eaten by the
/// editor. Text that no longer fits scrolls inside the box.
pub fn input_box_height(app: &App, total_width: usize, available_height: u16) -> u16 {
    input_height(app, total_width)
        .saturating_add(2 * INPUT_VPAD)
        .min(available_height)
}

/// Height of the status line under the input box. It only exists while it has
/// something to say, so an idle prompt gives the row back to the transcript.
pub fn footer_height(app: &App) -> u16 {
    u16::from(app.fresh_status().is_some() || app.queued.total() > 0 || app.is_paused())
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
        | AgentStatus::Stopping
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
    // Paint the padded panel first; the text sits inside it.
    let panel = Style::default().bg(theme.bg);
    f.render_widget(Paragraph::new("").style(panel), area);
    let text_area = if area.height > 2 * INPUT_VPAD {
        area.inner(Margin::new(0, INPUT_VPAD))
    } else {
        area
    };

    let show_cursor = app.focus == FocusPane::Input && app.agent_status == AgentStatus::Idle;

    let total_width = area.width.max(1) as usize;
    let rows = input_row_ranges(app, total_width);
    let (cursor_row, cursor_offset) = input_cursor_position(app, total_width);
    let visible = text_area.height.max(1) as usize;
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

    let paragraph = Paragraph::new(lines).style(panel);
    f.render_widget(paragraph, text_area);
}

pub fn render_status_bar(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if area.height == 0 {
        return;
    }

    // No static command tips: this line only carries transient state, and the
    // layout collapses it to zero rows when there is none.
    let hints: Vec<Span> = match app.fresh_status() {
        Some(msg) => vec![
            Span::styled(" ⚡ ", Style::default().fg(theme.highlight)),
            Span::styled(
                msg.to_string(),
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
        ],
        None => {
            let mut hints: Vec<Span> = Vec::new();
            if app.queued.total() > 0 {
                hints.push(Span::styled(
                    format!(" 📥 {} queued", app.queued.total()),
                    Style::default()
                        .fg(theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if app.is_paused() {
                hints.push(Span::styled(
                    " ⏸ paused",
                    Style::default().fg(theme.error_color),
                ));
            }
            hints
        }
    };

    let paragraph =
        Paragraph::new(Line::from(hints)).style(Style::default().bg(Color::Rgb(10, 14, 20)));

    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_input(input: &str) -> App {
        let mut app = App::new("gpt-4o");
        app.set_input(input.to_string());
        app
    }

    #[test]
    fn input_rows_count_newlines_and_soft_wraps() {
        assert_eq!(input_height(&app_with_input("one line"), 60), 1);
        assert_eq!(input_height(&app_with_input("one\ntwo\nthree"), 60), 3);
        assert!(
            input_height(&app_with_input(&"x".repeat(200)), 40) > 1,
            "a line wider than the pane wraps"
        );
    }

    #[test]
    fn the_box_is_capped_and_padded() {
        let many_lines = "line\n".repeat(20);
        let tall = app_with_input(many_lines.trim_end_matches('\n'));
        assert_eq!(input_height(&tall, 60), MAX_INPUT_ROWS as u16);
        assert_eq!(
            input_box_height(&tall, 60, 100),
            MAX_INPUT_ROWS as u16 + 2 * INPUT_VPAD
        );
        // A short frame caps the box, so the editor can never eat the screen.
        assert_eq!(input_box_height(&tall, 60, 4), 4);
        assert_eq!(input_box_height(&app_with_input("one"), 60, 4), 3);
    }

    #[test]
    fn the_footer_only_exists_when_it_has_something_to_say() {
        let mut app = App::new("gpt-4o");
        assert_eq!(footer_height(&app), 0, "an idle prompt reclaims the row");
        app.set_status_message("hello");
        assert_eq!(
            footer_height(&app),
            1,
            "a fresh status message takes it back"
        );
    }
}
