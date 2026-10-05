use crate::app::{AgentStatus, App};
use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
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
        AgentStatus::Stopping => Span::styled(
            " ⏹ STOPPING ",
            Style::default()
                .fg(theme.error_color)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Error(_) => Span::styled(
            " ✖ ERROR ",
            Style::default()
                .fg(theme.error_color)
                .add_modifier(Modifier::BOLD),
        ),
    };

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
        if let Some(tl) = app.effective_thinking_level() {
            Span::styled(format!("[think:{}] ", tl), theme.muted_style())
        } else {
            Span::raw("")
        },
        if app.is_paused() {
            Span::styled(
                " ⏸ PAUSED ",
                Style::default()
                    .fg(theme.error_color)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            status_span
        },
    ]);

    // Token and turn counts deliberately live in the metrics bar only, and the
    // session name lives in the footer: this row is one line tall, and crowding
    // it pushed the line past the pane width, truncating whatever sat at its end.
    let paragraph = Paragraph::new(vec![left])
        // No block: a bordered block on a one-row area leaves nothing to draw in,
        // which is why this line used to be an anonymous rule. The chat pane's top
        // border sits directly below and already separates the two.
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}
