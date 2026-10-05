use crate::app::{ActiveToolCall, AgentStatus, App, LinkHitbox, TimelineMark};
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
///
/// A line is not a screen row: lines pushed verbatim (tool output, system
/// notices, tool-call args) can be wider than the pane and get word-wrapped by
/// `Paragraph` into several rows. `row_end` is the cumulative screen-row map
/// that keeps click hitboxes on the row the pane actually paints.
struct Stream {
    width: u16,
    lines: Vec<Line<'static>>,
    links: Vec<LinkSpan>,
    /// Screen rows occupied by `lines[..=i]`, one entry per line.
    row_end: Vec<usize>,
}

/// A user turn as it is discovered while building the stream. `line` is the
/// logical line the turn starts on; it becomes a screen row only once the whole
/// stream has been measured, because wrapping decides how many rows precede it.
struct PendingTurn {
    index: usize,
    line: usize,
    prompt: String,
    tools: usize,
}

impl Stream {
    fn new(width: u16) -> Self {
        Self {
            width,
            lines: Vec::new(),
            links: Vec::new(),
            row_end: Vec::new(),
        }
    }

    /// Screen rows the pane paints a single line on. Measured with the very
    /// wrapper `Paragraph` renders with, so the two can never disagree.
    fn rows(line: &Line<'static>, width: u16) -> usize {
        Paragraph::new(vec![line.clone()])
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
    }

    fn push(&mut self, line: Line<'static>) {
        let rows = Self::rows(&line, self.width);
        let end = self.row_end.last().copied().unwrap_or(0) + rows;
        self.row_end.push(end);
        self.lines.push(line);
    }

    /// A line that carries no links (headers, tool output, system notices).
    fn raw(&mut self, line: Line<'static>) {
        self.push(line);
    }

    /// Appends a rendered block, rebasing its link line indices.
    fn append(&mut self, rendered: Rendered) {
        let offset = self.lines.len();
        for line in rendered.lines {
            self.push(line);
        }
        for mut link in rendered.links {
            link.line += offset;
            self.links.push(link);
        }
    }
}

/// Screen row the line at `index` starts on, from [`Stream`]'s cumulative map.
fn rows_before(row_end: &[usize], index: usize) -> usize {
    index
        .checked_sub(1)
        .and_then(|prev| row_end.get(prev).copied())
        .unwrap_or(0)
}

/// First non-empty line of a prompt, cut to something that fits a one-row hint.
fn prompt_excerpt(content: &str) -> String {
    const MAX: usize = 60;
    let line = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let mut excerpt = thunder_agent_providers::naming::truncate_chars(line, MAX);
    if line.chars().count() > MAX {
        excerpt.push('…');
    }
    excerpt
}

/// Whether a user message is the UI echoing one of its own slash commands back
/// into the transcript. Those are bookkeeping, not turns the agent answered, so
/// the rail does not chart them.
fn is_local_command_echo(content: &str) -> bool {
    let token = content.split_whitespace().next().unwrap_or("");
    token.starts_with('/')
        && crate::commands::ALL_COMMANDS
            .iter()
            .any(|cmd| cmd.matches(token))
}

pub fn render_chat(f: &mut Frame, app: &mut App, area: Rect, theme: &Theme) {
    let width = area.width.max(1) as usize;
    let markdown_on = app.markdown_enabled;
    let roots = app.workspace_roots();
    let raw_style = Style::default().fg(theme.text_main);

    let mut stream = Stream::new(area.width.max(1));
    // The turns the rail charts, and the tool calls the current one has issued
    // so far (closed out when the next user message arrives).
    let mut turns: Vec<PendingTurn> = Vec::new();
    let mut turn_tools = 0usize;
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
                    // A new user message closes the previous turn's tool count.
                    if let Some(previous) = turns.last_mut() {
                        previous.tools = turn_tools;
                    }
                    turn_tools = 0;
                    if !is_local_command_echo(content) {
                        turns.push(PendingTurn {
                            index: turns.len() + 1,
                            line: stream.lines.len(),
                            prompt: prompt_excerpt(content),
                            tools: 0,
                        });
                    }
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
                        turn_tools += calls.len();
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
        render_active_tool_call(&mut stream, tc, theme);
        stream.raw(Line::raw(""));
    }

    // A logical line can be several screen rows once `Paragraph` wraps it, so
    // both the scroll bound and the click hitboxes below are expressed in the
    // screen rows the stream measured as it was built.
    let links = std::mem::take(&mut stream.links);
    let row_end = std::mem::take(&mut stream.row_end);

    // Close out the last turn, then turn line indices into the rows the pane
    // actually paints on: the rail jumps by row, and only this map knows them.
    if let Some(last) = turns.last_mut() {
        last.tools = turn_tools;
    }
    let marks: Vec<TimelineMark> = turns
        .iter()
        .map(|turn| TimelineMark {
            index: turn.index,
            row: rows_before(&row_end, turn.line),
            prompt: turn.prompt.clone(),
            tools: turn.tools,
            duration_ms: app
                .turn_durations
                .get(turn.index.saturating_sub(1))
                .copied()
                .flatten(),
        })
        .collect();
    app.timeline_marks = marks;

    let paragraph = Paragraph::new(stream.lines).wrap(Wrap { trim: false });
    let total_rendered_lines = row_end.last().copied().unwrap_or(0);
    debug_assert_eq!(
        total_rendered_lines,
        paragraph.line_count(area.width),
        "the stream's row map must agree with the renderer's own wrapping"
    );

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
        links
            .iter()
            .filter_map(|link| {
                let first_row = rows_before(&row_end, link.line);
                if first_row < scroll_y || first_row >= scroll_y + visible_height {
                    return None;
                }
                Some(LinkHitbox {
                    // The `Block` draws a top border, so content starts one row in.
                    row: area.y + 1 + (first_row - scroll_y) as u16,
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

    // `Paragraph::scroll` takes a `u16`; a transcript taller than that cannot be
    // addressed exactly, so clamp instead of wrapping around to the top.
    let scroll_row = scroll_y.min(u16::MAX as usize) as u16;

    f.render_widget(paragraph.block(block).scroll((scroll_row, 0)), area);
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

fn render_active_tool_call(stream: &mut Stream, tc: &ActiveToolCall, theme: &Theme) {
    let status_str = if tc.result.is_some() {
        if tc.is_error {
            "✖ Failed"
        } else {
            "✔ Completed"
        }
    } else {
        "⏳ Running..."
    };

    stream.raw(Line::from(vec![
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
        stream.raw(Line::from(vec![
            Span::raw("    └─ "),
            Span::styled(short_res, theme.muted_style()),
        ]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row map is built one line at a time; the pane wraps the whole
    /// transcript in one pass. If those ever disagree, click hitboxes drift.
    #[test]
    fn per_line_rows_sum_to_the_whole_paragraph() {
        let width = 80u16;
        let lines: Vec<Line<'static>> = vec![
            Line::raw(""),
            Line::raw("short"),
            Line::raw(" ".repeat(80)),
            Line::raw("a".repeat(80)),
            Line::raw("a".repeat(81)),
            Line::raw("中".repeat(41)),
            Line::raw("wrapped words ".repeat(30)),
            Line::raw(format!("{}   ", "t".repeat(78))),
            Line::from(vec![Span::raw("x".repeat(40)), Span::raw("y".repeat(40))]),
        ];

        let mut stream = Stream::new(width);
        for line in lines.iter().cloned() {
            stream.raw(line);
        }

        let whole = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .line_count(width);
        assert_eq!(stream.row_end.len(), stream.lines.len());
        assert_eq!(stream.row_end.last().copied().unwrap_or(0), whole);
        // The map is strictly increasing, and line 0 starts at row 0.
        assert!(stream.row_end.windows(2).all(|w| w[1] > w[0]));
        assert_eq!(rows_before(&stream.row_end, 0), 0);
        assert_eq!(rows_before(&stream.row_end, 1), stream.row_end[0]);
    }
}
