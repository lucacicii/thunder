use crate::app::{App, FocusPane};
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};
use ratatui::Frame;

pub fn render_sidebar(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let is_focused = app.focus == FocusPane::Sidebar;
    let mut items: Vec<ListItem> = Vec::new();

    if app.session_list.is_empty() {
        items.push(ListItem::new(vec![
            Line::styled(" (No saved sessions)", theme.muted_style()),
            Line::styled(" Press Ctrl+N for new", theme.muted_style()),
        ]));
    } else {
        for (idx, summary) in app.session_list.iter().enumerate() {
            let is_current = summary.id == app.conversation.id;
            let is_selected = idx == app.selected_session_idx;

            let cursor_prefix = if is_selected { "❯ " } else { "  " };
            let status_badge = if is_current { "● " } else { "○ " };
            let title = summary.title.as_deref().unwrap_or(&summary.id);

            let item_bg = if is_selected && is_focused {
                Color::Rgb(38, 52, 75)
            } else if is_selected {
                Color::Rgb(28, 35, 48)
            } else if is_current {
                Color::Rgb(20, 26, 36)
            } else {
                Color::Reset
            };

            let title_style = if is_current {
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else if is_selected {
                Style::default()
                    .fg(theme.text_main)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text_muted)
            };

            let line1 = Line::from(vec![
                Span::styled(
                    cursor_prefix,
                    if is_selected {
                        Style::default()
                            .fg(theme.accent_primary)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        theme.muted_style()
                    },
                ),
                Span::styled(
                    status_badge,
                    if is_current {
                        Style::default().fg(theme.accent_primary)
                    } else {
                        theme.muted_style()
                    },
                ),
                Span::styled(title, title_style),
            ]);

            let line2 = Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    format!(
                        "msgs: {} | ~{} tok",
                        summary.message_count, summary.total_tokens
                    ),
                    Style::default()
                        .fg(theme.text_muted)
                        .add_modifier(Modifier::DIM),
                ),
            ]);

            items.push(
                ListItem::new(vec![line1, line2, Line::raw("")])
                    .style(Style::default().bg(item_bg)),
            );
        }
    }

    let title_text = if is_focused {
        " 📁 Sessions (Enter: Load, d: Del) "
    } else {
        " 📁 Sessions "
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title_text)
        .title_style(theme.title_style())
        .border_style(theme.focus_border(is_focused));

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}
