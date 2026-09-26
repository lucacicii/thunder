use crate::app::{AgentStatus, App, FocusPane};
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render_input(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let is_focused = app.focus == FocusPane::Input;

    let prompt_prefix = match app.agent_status {
        AgentStatus::Idle => Span::styled(
            "❯ ",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Thinking => Span::styled(
            "◐ ",
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Streaming => Span::styled(
            "▶ ",
            Style::default()
                .fg(theme.assistant_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::ExecutingTool { .. } => Span::styled(
            "⚙ ",
            Style::default()
                .fg(theme.tool_bubble)
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
    };

    let input_text = if app.input.is_empty() {
        if is_focused {
            Span::styled(
                "Type a message, or / for commands (e.g. /resume, /model, /skills)...",
                Style::default().fg(Color::Rgb(100, 116, 139)),
            )
        } else {
            Span::styled("Press Tab to focus input...", theme.muted_style())
        }
    } else {
        Span::styled(&app.input, theme.text_style())
    };

    let cursor_span = if is_focused && app.agent_status == AgentStatus::Idle {
        Span::styled("█", Style::default().fg(theme.accent_primary))
    } else {
        Span::raw("")
    };

    let paragraph = Paragraph::new(Line::from(vec![prompt_prefix, input_text, cursor_span]))
        .style(Style::default().bg(Color::Rgb(13, 17, 23)));

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
        Span::styled("Ctrl+C / Esc", Style::default().fg(theme.text_muted)),
        Span::styled(" Cancel", theme.muted_style()),
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
