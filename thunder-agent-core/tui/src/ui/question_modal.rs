//! The `ask_user_question` modal.
//!
//! Renders the pending question (one at a time, queue-aware) above the chat:
//! selectable options as a list (Space toggles in multi-select), free-form
//! questions with an inline input line. Esc dismisses the whole question.

use crate::app::App;
use crate::ui::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
    let x = (r.width.saturating_sub(popup_width)) / 2;
    let y = (r.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}

pub fn render_question_modal(f: &mut Frame, app: &App, theme: &Theme) {
    let Some(pending) = app.pending_question.as_ref() else {
        return;
    };
    let Some(current) = pending.current() else {
        return;
    };

    let modal_area = centered_rect(70, 60, f.area());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " ❓ Agent Question ({}/{}{} — agent is waiting) ",
            pending.answered_so_far().len() + 1,
            pending.answered_so_far().len() + pending.remaining(),
            if pending.remaining() > 1 {
                " questions"
            } else {
                ""
            }
        ))
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.highlight));
    f.render_widget(Clear, modal_area);
    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // question
            Constraint::Min(3),    // options / input
            Constraint::Length(1), // collected answers
            Constraint::Length(1), // footer
        ])
        .split(inner);

    // Header line
    let header = current
        .header
        .clone()
        .unwrap_or_else(|| "Question".to_string());
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " 🎭 ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                header,
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            if current.multi_select {
                Span::styled("  (multi-select: Space toggles)", theme.muted_style())
            } else {
                Span::raw("")
            },
        ])),
        chunks[0],
    );

    // Question text
    f.render_widget(
        Paragraph::new(Span::styled(current.question.clone(), theme.text_style()))
            .wrap(Wrap { trim: true }),
        chunks[1],
    );

    // Options list or free-form input
    if current.options.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ❯ ", Style::default().fg(theme.accent_primary)),
                Span::styled(pending.input.clone(), theme.text_style()),
                Span::styled("█", Style::default().fg(theme.accent_primary)),
            ])),
            chunks[2],
        );
    } else {
        let visible_rows = chunks[2].height.max(1) as usize;
        let start = pending
            .selected
            .saturating_sub(visible_rows.saturating_sub(1));
        let end = (start + visible_rows).min(current.options.len());

        let mut lines = Vec::new();
        for (idx, opt) in current.options[start..end].iter().enumerate() {
            let actual = start + idx;
            let is_selected = actual == pending.selected;
            let is_toggled = pending.toggled.contains(&actual);

            let marker = if current.multi_select {
                if is_toggled {
                    " ◉ "
                } else {
                    " ○ "
                }
            } else if is_selected {
                " ▶ "
            } else {
                "   "
            };

            let mut spans = vec![
                Span::styled(
                    marker,
                    if is_selected || is_toggled {
                        Style::default()
                            .fg(theme.highlight)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.muted_style()
                    },
                ),
                Span::styled(
                    opt.label.clone(),
                    if is_selected || is_toggled {
                        Style::default()
                            .fg(theme.text_main)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.text_style()
                    },
                ),
            ];
            if let Some(desc) = &opt.description {
                spans.push(Span::styled(format!("  — {desc}"), theme.muted_style()));
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), chunks[2]);
    }

    // Already-collected answers
    let collected = if pending.answered_so_far().is_empty() {
        Line::raw("")
    } else {
        Line::from(Span::styled(
            format!(
                " ✔ {}",
                pending
                    .answered_so_far()
                    .iter()
                    .map(|(q, a)| format!("{q} → {a}"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            ),
            theme.muted_style(),
        ))
    };
    f.render_widget(collected, chunks[3]);

    // Footer hints
    let hints = if current.options.is_empty() {
        " Enter submit · Esc dismiss "
    } else if current.multi_select {
        " ↑/↓ move · Space toggle · Enter submit · Esc dismiss "
    } else {
        " ↑/↓ move · Enter select · Esc dismiss "
    };
    f.render_widget(
        Paragraph::new(Span::styled(hints, Style::default().fg(theme.text_muted))),
        chunks[4],
    );
}
