use crate::app::{ActiveToolCall, AgentStatus, App, LinkHitbox};
use crate::ui::markdown::{self, LinkSpan, RenderCtx, Rendered};
use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use thunder_agent_loop::types::message::ChatMessage;

/// The transcript being assembled: rendered lines plus the link hitboxes found
/// while building them, with line indices kept global across messages.
#[derive(Default)]
struct Stream {
    lines: Vec<Line<'static>>,
    links: Vec<LinkSpan>,
}

impl Stream {
    /// A line that carries no links (headers, tool output, system notices).
    fn raw(&mut self, line: Line<'static>) {
        self.lines.push(line);
    }

    /// Appends a rendered block, rebasing its link line indices.
    fn append(&mut self, rendered: Rendered) {
        let offset = self.lines.len();
        self.lines.extend(rendered.lines);
        for mut link in rendered.links {
            link.line += offset;
            self.links.push(link);
        }
    }
}

pub fn render_chat(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let width = area.width.max(1) as usize;
    let markdown_on = app.markdown_enabled;
    let roots = app.workspace_roots();
    let raw_style = Style::default().fg(theme.text_main);

    let mut stream = Stream::default();
    {
        let mut ctx = RenderCtx {
            theme,
            width,
            roots: &roots,
            cache: &mut app.link_cache,
        };

        // 1. Committed messages, in dialogue order.
        for msg in &app.conversation.messages {
            match msg {
                ChatMessage::System { content, .. } => {
                    stream.raw(Line::from(vec![
                        Span::styled(
                            "⚙ System: ",
                            Style::default()
                                .fg(theme.text_muted)
                                .add_modifier(Modifier::DIM),
                        ),
                        Span::styled(
                            content.replace('\n', " "),
                            Style::default()
                                .fg(theme.text_muted)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                    stream.raw(Line::raw(""));
                }
                ChatMessage::User { content, parts, .. } => {
                    stream.raw(Line::from(vec![Span::styled(
                        "👤 You",
                        Style::default()
                            .fg(theme.user_bubble)
                            .add_modifier(Modifier::BOLD),
                    )]));
                    if let Some(parts) = parts {
                        let images = parts.iter().filter(|p| p.is_image()).count();
                        if images > 0 {
                            stream.raw(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(
                                    format!("🖼 {images} image(s)"),
                                    Style::default()
                                        .fg(theme.tool_bubble)
                                        .add_modifier(Modifier::ITALIC),
                                ),
                            ]));
                        }
                    }
                    push_body(&mut stream, &mut ctx, markdown_on, content, raw_style);
                    stream.raw(Line::raw(""));
                }
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                    ..
                } => {
                    stream.raw(Line::from(vec![Span::styled(
                        "⚡ Thunder Assistant",
                        Style::default()
                            .fg(theme.assistant_bubble)
                            .add_modifier(Modifier::BOLD),
                    )]));
                    if let Some(c) = content {
                        push_body(&mut stream, &mut ctx, markdown_on, c, raw_style);
                    }
                    if let Some(calls) = tool_calls {
                        for call in calls {
                            stream.raw(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(
                                    format!("⚙️ Tool Call: `{}`", call.function.name),
                                    Style::default().fg(theme.tool_bubble),
                                ),
                                Span::styled(
                                    format!(" args: {}", call.function.arguments),
                                    Style::default().fg(theme.text_muted),
                                ),
                            ]));
                        }
                    }
                    stream.raw(Line::raw(""));
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                    name,
                } => {
                    let tool_name = name.as_deref().unwrap_or("tool");
                    stream.raw(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("🔧 [{tool_name}] (id: {tool_call_id})"),
                            Style::default()
                                .fg(theme.tool_bubble)
                                .add_modifier(Modifier::DIM),
                        ),
                    ]));

                    // Tool output is logs and diffs, not prose: rendered as-is.
                    for (idx, line) in content.lines().enumerate() {
                        if idx >= 8 {
                            stream.raw(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(
                                    "... (output truncated for display)",
                                    theme.muted_style(),
                                ),
                            ]));
                            break;
                        }
                        stream.raw(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                line.to_string(),
                                Style::default().fg(Color::Rgb(148, 163, 184)),
                            ),
                        ]));
                    }
                    stream.raw(Line::raw(""));
                }
            }
        }

        // 2. In-progress reasoning delta for the active turn.
        if !app.reasoning_delta.is_empty() {
            stream.raw(Line::from(vec![Span::styled(
                "🧠 Thinking Process:",
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::BOLD),
            )]));
            if markdown_on {
                stream.append(markdown::render(&app.reasoning_delta, &mut ctx));
            } else {
                for line in app.reasoning_delta.lines() {
                    stream.raw(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            line.to_string(),
                            Style::default()
                                .fg(Color::Rgb(160, 174, 192))
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }
            stream.raw(Line::raw(""));
        }

        // 3. In-progress streaming delta for the active turn.
        if !app.streaming_delta.is_empty()
            || app.agent_status == AgentStatus::Streaming
            || app.agent_status == AgentStatus::Thinking
        {
            if app.streaming_delta.is_empty() && app.agent_status == AgentStatus::Thinking {
                stream.raw(Line::from(vec![
                    Span::styled(
                        "⚡ Thunder Assistant ",
                        Style::default()
                            .fg(theme.assistant_bubble)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "◐ (reasoning & planning...)",
                        Style::default()
                            .fg(theme.tool_bubble)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            } else {
                stream.raw(Line::from(vec![Span::styled(
                    "⚡ Thunder Assistant",
                    Style::default()
                        .fg(theme.assistant_bubble)
                        .add_modifier(Modifier::BOLD),
                )]));
                push_body(
                    &mut stream,
                    &mut ctx,
                    markdown_on,
                    &app.streaming_delta,
                    raw_style,
                );
            }
            stream.raw(Line::raw(""));
        }
    }

    // 4. In-progress active tool calls for the current turn.
    for tc in &app.active_tool_calls {
        render_active_tool_call(&mut stream.lines, tc, theme);
        stream.raw(Line::raw(""));
    }

    // Scroll bounds. In Markdown mode every rendered line is exactly one screen
    // row, so the count is exact; the raw path still relies on `Paragraph`'s
    // wrapping, which has to be estimated.
    let inner_width = (area.width.saturating_sub(2) as usize).max(1);
    let total_rendered_lines = if markdown_on {
        stream.lines.len()
    } else {
        stream
            .lines
            .iter()
            .map(|line| {
                let line_len: usize = line
                    .spans
                    .iter()
                    .map(|s| str_display_width(&s.content))
                    .sum();
                if line_len == 0 {
                    1
                } else {
                    line_len.div_ceil(inner_width)
                }
            })
            .sum()
    };

    let visible_height = area.height.saturating_sub(2) as usize;
    let max_scroll = total_rendered_lines.saturating_sub(visible_height);
    app.last_max_scroll = max_scroll;

    let scroll_y = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    // Project link hitboxes into screen coordinates for this frame's viewport.
    app.link_hitboxes = if markdown_on {
        stream
            .links
            .iter()
            .filter_map(|link| {
                if link.line < scroll_y || link.line >= scroll_y + visible_height {
                    return None;
                }
                Some(LinkHitbox {
                    // The `Block` draws a top border, so content starts one row in.
                    row: area.y + 1 + (link.line - scroll_y) as u16,
                    col_start: area.x + link.col_start,
                    col_end: area.x + link.col_end,
                    target: link.target.clone(),
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    let title = if app.auto_scroll {
        " ⚡ Chat Stream (Auto-Scroll) ".to_string()
    } else {
        format!(
            " ⚡ Chat Stream [Scroll: {}/{} - Press End to snap bottom] ",
            scroll_y, max_scroll
        )
    };

    let mut block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .title(title)
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.border_normal));

    // The busy indicator sits on the separator directly above the prompt, so it
    // is always in view without competing with what the user is typing.
    if app.is_running() {
        let label = if app.agent_status == AgentStatus::Stopping {
            "stopping…"
        } else {
            "working…"
        };
        block = block.title_bottom(
            Line::from(Span::styled(
                format!("{} {label} ", app.spinner()),
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::ITALIC),
            ))
            .alignment(Alignment::Right),
        );
    }

    let paragraph = Paragraph::new(stream.lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y as u16, 0));

    f.render_widget(paragraph, area);
}

/// Renders message body text, or the raw source when preview is off.
fn push_body(
    stream: &mut Stream,
    ctx: &mut RenderCtx<'_>,
    markdown_on: bool,
    text: &str,
    raw_style: Style,
) {
    if markdown_on {
        stream.append(markdown::render(text, ctx));
    } else {
        for line in text.lines() {
            stream.raw(Line::from(vec![
                Span::raw("  "),
                Span::styled(line.to_string(), raw_style),
            ]));
        }
    }
}

fn str_display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    UnicodeWidthStr::width(s)
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
        Span::styled(
            format!("⚙ Executing Tool `{}`: ", tc.name),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            status_str,
            Style::default().fg(if tc.is_error {
                theme.error_color
            } else {
                theme.assistant_bubble
            }),
        ),
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
