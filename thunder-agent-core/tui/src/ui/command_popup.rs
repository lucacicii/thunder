use crate::app::App;
use crate::commands::filter_commands;
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

const MAX_VISIBLE_COMMANDS: usize = 8;

pub fn render_command_popup(f: &mut Frame, app: &App, input_area: Rect, theme: &Theme) {
    if !app.input.starts_with('/') {
        return;
    }

    let matches = filter_commands(&app.input);
    if matches.is_empty() {
        return;
    }

    let visible = matches.len().min(MAX_VISIBLE_COMMANDS);
    let popup_height = (visible as u16) + 2;
    let popup_width = input_area
        .width
        .max(50)
        .min(f.area().width.saturating_sub(4));

    let x = input_area.x;
    let y = input_area.y.saturating_sub(popup_height);

    let popup_area = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };

    f.render_widget(Clear, popup_area);

    let selected_idx = app.command_popup_idx % matches.len();
    let start_idx = scroll_offset(selected_idx, matches.len(), visible);
    let end_idx = (start_idx + visible).min(matches.len());

    let mut lines = Vec::new();
    for (idx, cmd) in matches[start_idx..end_idx].iter().enumerate() {
        let actual_idx = start_idx + idx;
        let is_selected = actual_idx == selected_idx;

        let cursor_span = if is_selected {
            Span::styled(
                " ▶ ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("   ")
        };

        let cmd_span = Span::styled(
            format!("/{}", cmd.name),
            if is_selected {
                Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            },
        );

        let args_span = if !cmd.args_hint.is_empty() {
            Span::styled(format!(" {}", cmd.args_hint), theme.muted_style())
        } else {
            Span::raw("")
        };

        let desc_span = Span::styled(
            format!("  — {}", cmd.description),
            if is_selected {
                Style::default().fg(Color::Rgb(220, 230, 242))
            } else {
                theme.muted_style()
            },
        );

        let line_style = if is_selected {
            Style::default().bg(Color::Rgb(30, 58, 95))
        } else {
            Style::default()
        };

        lines.push(Line::from(vec![cursor_span, cmd_span, args_span, desc_span]).style(line_style));
    }

    let more = if matches.len() > visible {
        format!(" [{}/{}] ", selected_idx + 1, matches.len())
    } else {
        String::new()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.accent_primary))
        .title(format!(" ⚡ Slash Commands{more}(↑/↓ select, Tab/Enter) "))
        .title_style(theme.title_style())
        .style(Style::default().bg(Color::Rgb(15, 20, 28)));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, popup_area);
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
    fn slash_popup_keeps_selection_visible() {
        assert_eq!(scroll_offset(0, 16, 8), 0);
        assert_eq!(scroll_offset(7, 16, 8), 0);
        assert_eq!(scroll_offset(8, 16, 8), 1);
        assert_eq!(scroll_offset(15, 16, 8), 8);
    }
}
