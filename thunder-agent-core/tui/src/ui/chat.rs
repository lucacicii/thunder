use crate::app::{ActiveToolCall, AgentStatus, App};
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use thunder_agent_loop::types::message::ChatMessage;

pub fn render_chat(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let mut lines: Vec<Line> = Vec::new();

    // 1. Render all committed conversation messages in exact chronological dialogue order
    for msg in &app.conversation.messages {
        match msg {
            ChatMessage::System { content, .. } => {
                lines.push(Line::from(vec![
                    Span::styled("⚙ System: ", Style::default().fg(theme.text_muted).add_modifier(Modifier::DIM)),
                    Span::styled(content.replace('\n', " "), Style::default().fg(theme.text_muted).add_modifier(Modifier::ITALIC)),
                ]));
                lines.push(Line::raw(""));
            }
            ChatMessage::User { content, .. } => {
                lines.push(Line::from(vec![
                    Span::styled("👤 You", Style::default().fg(theme.user_bubble).add_modifier(Modifier::BOLD)),
                ]));
                for line in content.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(line, Style::default().fg(theme.text_main)),
                    ]));
                }
                lines.push(Line::raw(""));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                lines.push(Line::from(vec![
                    Span::styled("⚡ Thunder Assistant", Style::default().fg(theme.assistant_bubble).add_modifier(Modifier::BOLD)),
                ]));
                if let Some(c) = content {
                    for line in c.lines() {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(line, Style::default().fg(theme.text_main)),
                        ]));
                    }
                }
                if let Some(calls) = tool_calls {
                    for call in calls {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(format!("⚙️ Tool Call: `{}`", call.function.name), Style::default().fg(theme.tool_bubble)),
                            Span::styled(format!(" args: {}", call.function.arguments), Style::default().fg(theme.text_muted)),
                        ]));
                    }
                }
                lines.push(Line::raw(""));
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
                name,
            } => {
                let tool_name = name.as_deref().unwrap_or("tool");
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("🔧 [{tool_name}] (id: {tool_call_id})"), Style::default().fg(theme.tool_bubble).add_modifier(Modifier::DIM)),
                ]));

                for (idx, line) in content.lines().enumerate() {
                    if idx >= 8 {
                        lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("... (output truncated for display)", theme.muted_style()),
                        ]));
                        break;
                    }
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(line.to_string(), Style::default().fg(Color::Rgb(148, 163, 184))),
                    ]));
                }
                lines.push(Line::raw(""));
            }
        }
    }

    // 2. Render in-progress reasoning delta for the active turn
    if !app.reasoning_delta.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("🧠 Thinking Process:", Style::default().fg(theme.tool_bubble).add_modifier(Modifier::BOLD)),
        ]));
        for line in app.reasoning_delta.lines() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(line, Style::default().fg(Color::Rgb(160, 174, 192)).add_modifier(Modifier::ITALIC)),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // 3. Render in-progress streaming response delta for the active turn
    if !app.streaming_delta.is_empty() || app.agent_status == AgentStatus::Streaming || app.agent_status == AgentStatus::Thinking {
        if app.streaming_delta.is_empty() && app.agent_status == AgentStatus::Thinking {
            lines.push(Line::from(vec![
                Span::styled("⚡ Thunder Assistant ", Style::default().fg(theme.assistant_bubble).add_modifier(Modifier::BOLD)),
                Span::styled("◐ (reasoning & planning...)", Style::default().fg(theme.tool_bubble).add_modifier(Modifier::ITALIC)),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("⚡ Thunder Assistant", Style::default().fg(theme.assistant_bubble).add_modifier(Modifier::BOLD)),
            ]));
            for line in app.streaming_delta.lines() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line, Style::default().fg(theme.text_main)),
                ]));
            }
        }
        lines.push(Line::raw(""));
    }

    // 4. Render in-progress active tool calls for the current turn
    for tc in &app.active_tool_calls {
        render_active_tool_call(&mut lines, tc, theme);
        lines.push(Line::raw(""));
    }

    // Calculate wrap and scroll bounds with CJK/Unicode character display width support
    let inner_width = (area.width.saturating_sub(2) as usize).max(1);
    let mut total_rendered_lines = 0;

    for line in &lines {
        let line_len: usize = line.spans.iter().map(|s| str_display_width(&s.content)).sum();
        let line_rows = if line_len == 0 {
            1
        } else {
            (line_len + inner_width - 1) / inner_width
        };
        total_rendered_lines += line_rows.max(1);
    }

    let visible_height = area.height.saturating_sub(2) as usize;
    let max_scroll = total_rendered_lines.saturating_sub(visible_height);
    app.last_max_scroll = max_scroll;

    let scroll_y = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    let title = if app.auto_scroll {
        " ⚡ Chat Stream (Auto-Scroll) ".to_string()
    } else {
        format!(" ⚡ Chat Stream [Scroll: {}/{} - Press End to snap bottom] ", scroll_y, max_scroll)
    };

    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title)
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.border_normal));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y as u16, 0));

    f.render_widget(paragraph, area);
}

fn str_display_width(s: &str) -> usize {
    s.chars()
        .map(|c| if c > '\u{7F}' { 2 } else { 1 })
        .sum()
}

fn render_active_tool_call(lines: &mut Vec<Line>, tc: &ActiveToolCall, theme: &Theme) {
    let status_str = if tc.result.is_some() {
        if tc.is_error {
            "✖ Failed"
        } else {
            "✔ Completed"
        }
    } else {
        "⏳ Running..."
    };

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("⚙ Executing Tool `{}`: ", tc.name), Style::default().fg(theme.tool_bubble).add_modifier(Modifier::BOLD)),
        Span::styled(status_str, Style::default().fg(if tc.is_error { theme.error_color } else { theme.assistant_bubble })),
    ]));

    if let Some(res) = &tc.result {
        let short_res = if res.chars().count() > 150 {
            let s: String = res.chars().take(150).collect();
            format!("{}...", s)
        } else {
            res.clone()
        };
        lines.push(Line::from(vec![
            Span::raw("    └─ "),
            Span::styled(short_res, theme.muted_style()),
        ]));
    }
}
