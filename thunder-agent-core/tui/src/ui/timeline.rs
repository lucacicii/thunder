//! The floating turn rail: one tick per user turn, pinned to the right edge of
//! the transcript and vertically centred on it.
//!
//! The rail is the transcript's index. Clicking a tick scrolls the pane so that
//! turn becomes the top visible row; the keyboard reaches the same places with
//! `Tab` (to focus the rail) plus `j`/`k` and `Enter`. Only the ticks are drawn
//! — details appear in a hint box while hovering — so the rail stays a few cells
//! wide and costs the transcript nothing but the columns it covers.

use crate::app::{AgentStatus, App, FocusPane, TimelineHitbox, TimelineMark};
use crate::ui::metrics::format_duration;
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

/// Width of the rail when turn numbers fit two digits.
pub const RAIL_WIDTH: u16 = 7;
/// Shortest rail, and the floor for a session with only a couple of turns.
const MIN_HEIGHT: u16 = 5;
/// Tallest rail: past this it scrolls internally rather than spanning the pane.
const MAX_HEIGHT: u16 = 20;
/// The rail never claims more than this fraction of the transcript's height.
const MAX_HEIGHT_NUM: u16 = 3;
const MAX_HEIGHT_DEN: u16 = 5;
/// Panes narrower than this get no rail at all: the transcript needs the room.
const MIN_PANE_WIDTH: u16 = 100;
/// Gap between the rail and the pane's right edge.
const RIGHT_MARGIN: u16 = 1;
/// Longest hover hint, in cells.
const HINT_MAX_WIDTH: u16 = 78;
/// Background of the keyboard's node. Matches the prompt selection, so a
/// highlighted row reads the same wherever it appears.
const SELECTION_BG: Color = Color::Rgb(30, 58, 95);

/// Width of the rail: [`RAIL_WIDTH`], plus a cell once turn numbers reach three
/// digits.
pub fn rail_width(turns: usize) -> u16 {
    let digits = turns.max(1).to_string().len().max(2) as u16;
    5 + digits
}

/// Where the rail floats inside the transcript. A zero-sized rect means "do not
/// draw": the pane is too narrow to share.
pub fn rail_rect(chat_area: Rect, turns: usize) -> Rect {
    if chat_area.width < MIN_PANE_WIDTH || chat_area.height == 0 {
        return Rect::default();
    }
    let cap = chat_area
        .height
        .saturating_mul(MAX_HEIGHT_NUM)
        .checked_div(MAX_HEIGHT_DEN)
        .unwrap_or(MIN_HEIGHT)
        .clamp(MIN_HEIGHT, MAX_HEIGHT)
        .min(chat_area.height);
    // The box hugs the ticks, so it reads as centred on the pane rather than as
    // a panel anchored to the top; past the cap the window scrolls instead.
    let height = u16::try_from(turns)
        .unwrap_or(u16::MAX)
        .max(MIN_HEIGHT)
        .min(cap);
    let width = rail_width(turns).min(chat_area.width);
    let x = chat_area
        .right()
        .saturating_sub(RIGHT_MARGIN + width)
        .max(chat_area.x);
    let y = chat_area.y + chat_area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// The slice of turns the rail shows, centred on `anchor` (0-based).
pub fn rail_window(turns: usize, anchor: usize, height: usize) -> (usize, usize) {
    if height == 0 {
        return (0, 0);
    }
    if turns <= height {
        return (0, turns);
    }
    let start = anchor.saturating_sub(height / 2).min(turns - height);
    (start, start + height)
}

pub fn render_timeline(f: &mut Frame, app: &mut App, chat_area: Rect, theme: &Theme) {
    app.timeline_hitboxes.clear();
    if !app.timeline_visible || app.timeline_marks.len() < 2 || chat_area.width < MIN_PANE_WIDTH {
        app.timeline_hover = None;
        return;
    }
    let area = rail_rect(chat_area, app.timeline_marks.len());
    if area.width == 0 || area.height == 0 {
        return;
    }

    let height = area.height as usize;
    let anchor = app
        .timeline_anchor()
        .map(|index| index.saturating_sub(1))
        .unwrap_or(0);
    let (start, end) = rail_window(app.timeline_marks.len(), anchor, height);
    let digits = (rail_width(app.timeline_marks.len()) - 5) as usize;

    // Opaque: the rail floats over prose, and prose must not read through it.
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default().style(Style::default().bg(theme.surface)),
        area,
    );

    let mut lines = Vec::with_capacity(end.saturating_sub(start));
    let mut hitboxes = Vec::with_capacity(end.saturating_sub(start));
    for (offset, mark) in app.timeline_marks[start..end].iter().enumerate() {
        hitboxes.push(TimelineHitbox {
            row: area.y + offset as u16,
            col_start: area.x,
            col_end: area.x + area.width,
            index: mark.index,
        });
        lines.push(node_line(app, mark, theme, digits));
    }
    f.render_widget(Paragraph::new(lines), area);
    app.timeline_hitboxes = hitboxes;

    if let Some(index) = app.timeline_hover {
        if let Some(mark) = app.timeline_marks.iter().find(|mark| mark.index == index) {
            render_hint(f, app, mark, chat_area, area, theme);
        }
    }
}

/// One tick: a slice of the vertical rail, the turn's state glyph and its
/// number.
fn node_line(app: &App, mark: &TimelineMark, theme: &Theme, digits: usize) -> Line<'static> {
    let last = app.timeline_marks.len();
    let running = app.is_running() && mark.index == last;
    let failed = matches!(app.agent_status, AgentStatus::Error(_)) && mark.index == last;
    let selected = app.timeline_selected == Some(mark.index);

    let (glyph, glyph_style) = if selected {
        (
            "●".to_string(),
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )
    } else if running {
        (
            app.spinner().to_string(),
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )
    } else if failed {
        ("✖".to_string(), theme.error_style())
    } else if app.timeline_hover == Some(mark.index) {
        ("●".to_string(), Style::default().fg(theme.accent_primary))
    } else {
        ("○".to_string(), theme.muted_style())
    };

    let number_style = if selected {
        Style::default()
            .fg(Color::Rgb(255, 255, 255))
            .add_modifier(Modifier::BOLD)
    } else {
        theme.muted_style()
    };

    let row_style = if selected && app.focus == FocusPane::Monitor {
        Style::default().bg(SELECTION_BG)
    } else {
        Style::default()
    };

    Line::from(vec![
        Span::raw(" "),
        Span::styled("│", Style::default().fg(theme.border_normal)),
        Span::raw(" "),
        Span::styled(glyph, glyph_style),
        Span::raw(" "),
        Span::styled(
            format!("{:>digits$}", mark.index, digits = digits),
            number_style,
        ),
    ])
    .style(row_style)
}

/// The details of the hovered node, in a one-row box under the rail. Floating
/// rather than in the status line: the layout must not shift on hover.
fn render_hint(
    f: &mut Frame,
    app: &App,
    mark: &TimelineMark,
    chat_area: Rect,
    rail: Rect,
    theme: &Theme,
) {
    let text = hint_text(app, mark);
    let width = (UnicodeWidthStr::width(text.as_str()) as u16 + 2)
        .min(HINT_MAX_WIDTH)
        .min(chat_area.width);
    if width == 0 {
        return;
    }
    let y = if rail.bottom() < chat_area.bottom() {
        rail.bottom()
    } else {
        rail.y.saturating_sub(1).max(chat_area.y)
    };
    let x = chat_area.right().saturating_sub(width).max(chat_area.x);
    let area = Rect::new(x, y, width, 1);

    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(text, theme.text_style()),
        ]))
        .style(Style::default().bg(theme.surface)),
        area,
    );
}

/// `#3 · done · 7 tools · 1m 02s · reshape the prompt box`.
fn hint_text(app: &App, mark: &TimelineMark) -> String {
    let last = app.timeline_marks.len();
    let status = if app.is_running() && mark.index == last {
        "running"
    } else if matches!(app.agent_status, AgentStatus::Error(_)) && mark.index == last {
        "failed"
    } else {
        "done"
    };
    let duration = mark
        .duration_ms
        .map(format_duration)
        .unwrap_or_else(|| "—".to_string());
    format!(
        "#{} · {} · {} tool{} · {} · {}",
        mark.index,
        status,
        mark.tools,
        if mark.tools == 1 { "" } else { "s" },
        duration,
        mark.prompt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_centres_on_the_anchor_within_the_turn_count() {
        // Everything fits: no scrolling at all.
        assert_eq!(rail_window(3, 1, 20), (0, 3));

        // The window follows the anchor without running off either end.
        assert_eq!(rail_window(100, 50, 10), (45, 55));
        assert_eq!(rail_window(100, 0, 10), (0, 10));
        assert_eq!(rail_window(100, 99, 10), (90, 100));
    }

    #[test]
    fn the_rail_keeps_its_margins_and_stays_inside_the_pane() {
        let pane = Rect::new(0, 2, 140, 30);
        let rail = rail_rect(pane, 12);
        assert_eq!(rail.width, RAIL_WIDTH);
        assert_eq!(rail.right(), pane.right() - RIGHT_MARGIN);
        assert!(rail.height >= MIN_HEIGHT && rail.height <= MAX_HEIGHT);
        // Vertically centred, within a cell.
        let above = rail.y - pane.y;
        let below = pane.bottom() - rail.bottom();
        assert!(above.abs_diff(below) <= 1, "above={above} below={below}");

        // A narrow pane gets no rail rather than a cramped one.
        assert_eq!(rail_rect(Rect::new(0, 0, 80, 30), 12), Rect::default());
    }

    #[test]
    fn three_digit_turns_widen_the_rail() {
        assert_eq!(rail_width(9), RAIL_WIDTH);
        assert_eq!(rail_width(120), RAIL_WIDTH + 1);
    }
}
