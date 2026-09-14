use crate::app::App;
use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn render_picker_modal(f: &mut Frame, app: &App, theme: &Theme) {
    if !app.picker.is_open {
        return;
    }

    let area = f.area();
    let modal_width = (area.width * 75 / 100).max(50).min(area.width.saturating_sub(4));
    let modal_height = (area.height * 70 / 100).max(12).min(area.height.saturating_sub(2));

    let x = (area.width.saturating_sub(modal_width)) / 2;
    let y = (area.height.saturating_sub(modal_height)) / 2;

    let modal_area = Rect {
        x,
        y,
        width: modal_width,
        height: modal_height,
    };

    f.render_widget(Clear, modal_area);

    let title = {
        let filtered = app.picker.filtered_items();
        let selected_idx = if filtered.is_empty() {
            0
        } else {
            app.picker.selected_index.min(filtered.len() - 1)
        };
        format!(
            " {} [{}/{}] ",
            app.picker.title,
            if filtered.is_empty() { 0 } else { selected_idx + 1 },
            filtered.len()
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focus))
        .title(title)
        .title_style(theme.title_style())
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(Color::Rgb(15, 20, 28)));

    let inner = block.inner(modal_area);
    f.render_widget(block, modal_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search
            Constraint::Length(1), // spacer
            Constraint::Min(3),    // list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let filtered = app.picker.filtered_items();
    let selected_idx = if filtered.is_empty() {
        0
    } else {
        app.picker.selected_index.min(filtered.len() - 1)
    };

    let filter_display = if app.picker.filter_text.is_empty() {
        Span::styled(
            " (type to search...) ",
            Style::default()
                .fg(Color::Rgb(100, 116, 139))
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::styled(
            format!(" \"{}\" ", app.picker.filter_text),
            Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
        )
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " 🔍 Search: ",
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            filter_display,
            Span::styled(format!(" [{} result(s)]", filtered.len()), theme.muted_style()),
        ])),
        chunks[0],
    );

    let visible_rows = chunks[2].height.max(1) as usize;
    let start_idx = scroll_offset(selected_idx, filtered.len(), visible_rows);
    let end_idx = (start_idx + visible_rows).min(filtered.len());

    let mut lines = Vec::new();
    if filtered.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "   No matching items found.",
            Style::default().fg(theme.error_color),
        )]));
    } else {
        for (rel_idx, item) in filtered[start_idx..end_idx].iter().enumerate() {
            let actual_idx = start_idx + rel_idx;
            let is_selected = actual_idx == selected_idx;

            let cursor_span = if is_selected {
                Span::styled(
                    " ▶ ",
                    Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("   ")
            };

            let title_span = Span::styled(
                format!("{:<28}", item.title),
                if is_selected {
                    Style::default()
                        .fg(Color::Rgb(255, 255, 255))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme.text_main)
                        .add_modifier(Modifier::BOLD)
                },
            );

            let badge_span = if let Some(badge) = &item.badge {
                Span::styled(
                    format!(" [{}] ", badge),
                    if is_selected {
                        Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.accent_secondary)
                    },
                )
            } else {
                Span::raw(" ")
            };

            let desc_span = Span::styled(
                item.description.lines().next().unwrap_or(""),
                if is_selected {
                    Style::default().fg(Color::Rgb(226, 232, 240))
                } else {
                    theme.muted_style()
                },
            );

            let row_style = if is_selected {
                Style::default().bg(Color::Rgb(30, 58, 95))
            } else {
                Style::default()
            };

            lines.push(
                Line::from(vec![cursor_span, title_span, badge_span, desc_span]).style(row_style),
            );
        }
    }

    f.render_widget(Paragraph::new(lines), chunks[2]);

    let more_above = if start_idx > 0 {
        format!(" ↑{} more ", start_idx)
    } else {
        String::new()
    };
    let more_below = if end_idx < filtered.len() {
        format!(" ↓{} more ", filtered.len() - end_idx)
    } else {
        String::new()
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(more_above, theme.muted_style()),
            Span::styled(" ↑/↓ move  Enter attach  Esc close ", theme.muted_style()),
            Span::styled(more_below, theme.muted_style()),
        ])),
        chunks[3],
    );
}

fn scroll_offset(selected_idx: usize, total: usize, visible_rows: usize) -> usize {
    if total == 0 || visible_rows == 0 || selected_idx < visible_rows {
        return 0;
    }
    let max_start = total.saturating_sub(visible_rows);
    selected_idx
        .saturating_sub(visible_rows.saturating_sub(1))
        .min(max_start)
}

#[cfg(test)]
mod tests {
    use super::scroll_offset;

    #[test]
    fn selected_item_stays_in_window() {
        assert_eq!(scroll_offset(0, 30, 8), 0);
        assert_eq!(scroll_offset(7, 30, 8), 0);
        assert_eq!(scroll_offset(8, 30, 8), 1);
        assert_eq!(scroll_offset(20, 30, 8), 13);
        assert_eq!(scroll_offset(29, 30, 8), 22);
    }
}
