use crate::app::{AgentStatus, App};
use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

pub fn render_header(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let status_span = match &app.agent_status {
        AgentStatus::Idle => Span::styled(" ● IDLE ", Style::default().fg(Color::DarkGray)),
        AgentStatus::Thinking => Span::styled(
            " ◐ THINKING... ",
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Streaming => Span::styled(
            " ▶ STREAMING ",
            Style::default()
                .fg(theme.assistant_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::ExecutingTool { name, .. } => Span::styled(
            format!(" [⚙ {name}] "),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Done => Span::styled(" ✔ DONE ", Style::default().fg(theme.assistant_bubble)),
        AgentStatus::Error(_) => Span::styled(
            " ✖ ERROR ",
            Style::default()
                .fg(theme.error_color)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let title_str = app.conversation.title.as_deref().unwrap_or("Untitled");
    let tokens_str = format!("Tokens: ~{}", app.conversation.stats.total_tokens);
    let turns_str = format!("Turns: {}", app.conversation.stats.turn_count);

    let left = Line::from(vec![
        Span::styled(
            "⚡ THUNDER ",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}] ", app.model.selection_id()),
            theme.muted_style(),
        ),
        Span::styled(
            format!("[{}] ", app.execution_mode.badge()),
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        if let Some(skill) = &app.active_skill {
            Span::styled(
                format!("[skill:{}] ", skill.name),
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
        status_span,
    ]);

    let center = Line::from(vec![
        Span::styled("Session: ", theme.muted_style()),
        Span::styled(
            title_str,
            Style::default()
                .fg(theme.text_main)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let right = Line::from(vec![
        Span::styled(tokens_str, theme.muted_style()),
        Span::styled(" | ", Style::default().fg(theme.border_normal)),
        Span::styled(turns_str, theme.muted_style()),
    ]);

    let block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(theme.border_normal));

    let paragraph = Paragraph::new(vec![Line::from(
        [
            left.spans.into_iter().collect::<Vec<_>>(),
            vec![Span::raw("   ")],
            center.spans.into_iter().collect::<Vec<_>>(),
            vec![Span::raw("   ")],
            right.spans.into_iter().collect::<Vec<_>>(),
        ]
        .concat(),
    )])
    .block(block)
    .alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}
