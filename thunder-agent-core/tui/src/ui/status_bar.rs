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

    // No placeholder: an empty input stays empty. The status bar already
    // carries the key hints, and the `❯` prompt marks the input line.
    let input_text = Span::styled(&app.input, theme.text_style());

    let cursor_span = if is_focused && app.agent_status == AgentStatus::Idle {
        Span::styled("█", Style::default().fg(theme.accent_primary))
    } else {
        Span::raw("")
    };

    // Staged image attachments for the next prompt (from `/image <path>`).
    let attachment_badge = if app.pending_images.is_empty() {
        Span::raw("")
    } else {
        Span::styled(
            format!("🖼 {} ", app.pending_images.len()),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        )
    };

    let paragraph = Paragraph::new(Line::from(vec![
        prompt_prefix,
        attachment_badge,
        input_text,
        cursor_span,
    ]))
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
