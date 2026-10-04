use crate::app::{AgentStatus, App, FocusPane};
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

pub fn render_input(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let is_focused = app.focus == FocusPane::Input;

    // While a run is live the prompt becomes an animated spinner and the line
    // says `working…`, so the agent's progress is visible where the eye already
    // is. The glyph advances on terminal ticks.
    let prompt_prefix = match app.agent_status {
        AgentStatus::Idle => Span::styled(
            "❯ ",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        AgentStatus::Thinking | AgentStatus::Streaming | AgentStatus::ExecutingTool { .. } => {
            Span::styled(
                format!("{} ", app.spinner()),
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::BOLD),
            )
        }
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

    let working = if app.is_running() {
        Span::styled(
            "working… ",
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::raw("")
    };

    // No placeholder: an empty input stays empty, with the caret marking the
    // spot. The text is split at the caret and the character under it is drawn
    // in reverse video, so nothing shifts as the caret moves.
    let show_cursor = is_focused && app.agent_status == AgentStatus::Idle;
    let chars: Vec<char> = app.input.chars().collect();
    let caret_at = app.input_cursor.min(chars.len());
    let before: String = chars[..caret_at].iter().collect();
    let at_caret = chars.get(caret_at).copied();

    let mut input_spans: Vec<Span> = Vec::new();
    if show_cursor {
        input_spans.push(Span::styled(before, theme.text_style()));
        input_spans.push(match at_caret {
            Some(c) => Span::styled(
                c.to_string(),
                Style::default()
                    .fg(theme.bg)
                    .bg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            None => Span::styled("█", Style::default().fg(theme.accent_primary)),
        });
        if at_caret.is_some() {
            let after: String = chars[caret_at + 1..].iter().collect();
            input_spans.push(Span::styled(after, theme.text_style()));
        }
    } else {
        input_spans.push(Span::styled(app.input.clone(), theme.text_style()));
    }

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

    let mut spans = vec![prompt_prefix, working, attachment_badge];
    spans.extend(input_spans);
    let paragraph =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(13, 17, 23)));

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
