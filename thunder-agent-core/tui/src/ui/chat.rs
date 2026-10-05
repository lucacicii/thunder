use crate::app::{
    thinking_key_of, AgentStatus, App, LinkHitbox, LiveKind, TimelineMark, ToolOutcome,
};
use crate::ui::markdown::{self, LinkSpan, RenderCtx, Rendered};
use crate::ui::metrics::format_duration;
use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use unicode_width::UnicodeWidthStr;

/// Output rows a collapsed tool line previews before `Ctrl + O` expands it.
const TOOL_PREVIEW_LINES: usize = 2;
/// Output rows an expanded tool line may paint before it is clipped.
const TOOL_EXPANDED_MAX_LINES: usize = 200;
/// One-line argument preview budget in an expanded tool block.
const TOOL_ARGS_CHARS: usize = 400;
/// Colour tool output is painted in, shared by the preview and the full view.
const TOOL_OUTPUT_COLOR: Color = Color::Rgb(148, 163, 184);

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

    /// Screen rows the pane paints a single line on.
    ///
    /// A line whose display width fits the pane is exactly one row — the word
    /// wrapper only breaks what is wider than the area. That cheap check covers
    /// the whole markdown path (which pre-wraps to the pane) and every short
    /// line; only an over-wide line is measured with the very wrapper
    /// `Paragraph` renders with, so the row map can never disagree with it.
    fn rows(line: &Line<'static>, width: u16) -> usize {
        let width = width.max(1) as usize;
        if Self::whitespace_only(line) {
            return Self::measure(line, width as u16);
        }
        let mut cells = 0usize;
        for span in &line.spans {
            cells += UnicodeWidthStr::width(span.content.as_ref());
            if cells > width {
                return Self::measure(line, width as u16);
            }
        }
        1
    }

    /// Whether the line is nothing but whitespace, which is the one case where
    /// fitting the pane is not one row: the wrapper emits a leading empty row
    /// for it. The zero-width space counts because the wrapper treats it as
    /// whitespace while `char::is_whitespace` does not; the non-breaking space
    /// is the opposite, and measuring it costs nothing.
    fn whitespace_only(line: &Line<'static>) -> bool {
        line.spans.iter().all(|span| {
            span.content
                .chars()
                .all(|c| c.is_whitespace() || c == '\u{200b}')
        })
    }

    /// Rows measured with the very wrapper `Paragraph` renders with.
    fn measure(line: &Line<'static>, width: u16) -> usize {
        Paragraph::new(vec![line.clone()])
            .wrap(Wrap { trim: false })
            .line_count(width)
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

/// Everything the transcript pane derives from the conversation: the rendered
/// lines, the screen-row map that positions scrolling and clicks, the link
/// spans and the rail marks.
///
/// Rebuilding this is linear in the whole conversation, while the pane is
/// redrawn far more often than the conversation changes (a keystroke, a scroll,
/// a mouse move, a tick). The pane therefore keeps the last build until one of
/// its inputs actually moves — see [`TranscriptKey`].
pub struct TranscriptCache {
    /// The inputs this build was made from, so a stale build is impossible.
    pub key: TranscriptKey,
    lines: Vec<Line<'static>>,
    /// Screen rows occupied by `lines[..=i]`, one entry per line.
    row_end: Vec<usize>,
    links: Vec<LinkSpan>,
    marks: Vec<TimelineMark>,
    /// Screen rows the whole transcript occupies once wrapped.
    total_rows: usize,
}

/// The inputs a rendered transcript depends on.
///
/// The counters are a safety net for direct `Conversation` edits: an append or
/// a wholesale replacement moves at least one of them, so even a mutation site
/// that forgets [`App::invalidate_transcript`] cannot leave a stale frame on
/// screen. The counters cannot see an in-place rewrite of an existing message,
/// which is why the mutation sites invalidate explicitly as well.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TranscriptKey {
    width: u16,
    markdown: bool,
    details: bool,
    /// Spinner frame, which the live tail paints into its thinking header.
    spinner: usize,
    roots: Vec<PathBuf>,
    messages: usize,
    streaming: usize,
    reasoning: usize,
    active_tools: usize,
    live_segments: usize,
    thinking: usize,
    outcomes: usize,
}

impl TranscriptKey {
    fn of(app: &App, width: u16) -> Self {
        Self {
            width,
            markdown: app.markdown_enabled,
            details: app.details_expanded,
            // Only a live run paints the spinner; while idle the counter must
            // not keep the cache from hitting.
            spinner: if app.is_running() {
                app.spinner_frame
            } else {
                0
            },
            roots: app.workspace_roots(),
            messages: app.conversation.messages.len(),
            streaming: app.streaming_delta.len(),
            reasoning: app.reasoning_delta.len(),
            active_tools: app.active_tool_calls.len(),
            live_segments: app.live_segments.len(),
            thinking: app.thinking.len(),
            outcomes: app.tool_outcomes.len(),
        }
    }
}

/// Index of the line whose screen rows contain `row`.
///
/// `row_end` is strictly increasing, so the first end that is greater than
/// `row` belongs to the line that covers it.
fn line_at_row(row_end: &[usize], row: usize) -> usize {
    row_end.partition_point(|end| *end <= row)
}

/// The half-open line range that covers screen rows `[scroll_row, scroll_row +
/// height)` of a cached transcript, so the pane is handed only what it paints.
fn visible_line_range(cache: &TranscriptCache, scroll_row: usize, height: usize) -> (usize, usize) {
    let len = cache.lines.len();
    if height == 0 || len == 0 || cache.total_rows == 0 {
        return (len, len);
    }
    let last_row = (scroll_row + height - 1).min(cache.total_rows - 1);
    let first = line_at_row(&cache.row_end, scroll_row).min(len);
    let end = (line_at_row(&cache.row_end, last_row) + 1).min(len);
    (first.min(end), end)
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
    // Rebuilding the transcript costs a pass over the whole conversation, while
    // this pane is redrawn for every keystroke, scroll and tick. Reuse the last
    // build unless one of its inputs moved.
    let key = TranscriptKey::of(app, area.width.max(1));
    if app.transcript.as_ref().map(|cache| &cache.key) != Some(&key) {
        app.transcript = Some(build_transcript(app, area.width.max(1), theme, key));
    }
    let cache = app.transcript.take().expect("a transcript was just built");
    paint_transcript(f, app, area, theme, &cache);
    app.transcript = Some(cache);
}

/// Renders the conversation as it stands now into a [`TranscriptCache`].
fn build_transcript(
    app: &mut App,
    area_width: u16,
    theme: &Theme,
    key: TranscriptKey,
) -> TranscriptCache {
    let width = area_width.max(1) as usize;
    let markdown_on = app.markdown_enabled;
    let roots = app.workspace_roots();
    let raw_style = Style::default().fg(theme.text_main);
    // Captured before the render context borrows `app.link_cache`, so the live
    // tail can still animate without borrowing the whole `App`.
    let spinner = app.spinner();

    let mut stream = Stream::new(area_width.max(1));
    // The turns the rail charts, and the tool calls the current one has issued
    // so far (closed out when the next user message arrives).
    let mut turns: Vec<PendingTurn> = Vec::new();
    let mut turn_tools = 0usize;
    // Display side tables, read while the transcript is walked: per-turn
    // thinking, finished tool outcomes, and the results their calls will claim.
    let thinking = &app.thinking;
    let outcomes = &app.tool_outcomes;
    let expanded = app.details_expanded;
    let referenced_tools: HashSet<&str> = app
        .conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => Some(calls.iter().map(|call| call.id.as_str())),
            _ => None,
        })
        .flatten()
        .collect();
    let tool_results: HashMap<&str, &str> = app
        .conversation
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::Tool {
                tool_call_id,
                content,
                ..
            } => Some((tool_call_id.as_str(), content.as_str())),
            _ => None,
        })
        .collect();
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
                // The system prompt is engine plumbing, not conversation: the
                // loop splices it into the request prefix, so the transcript
                // shows only what the user and the assistant actually said.
                ChatMessage::System { .. } => {}
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
                    // The engine's transcript carries no reasoning, so the turn's
                    // thinking is re-attached here, above the answer it preceded.
                    if let Some(blob) = thinking_key_of(msg).and_then(|key| thinking.get(&key)) {
                        push_thinking(
                            &mut stream,
                            &mut ctx,
                            markdown_on,
                            blob,
                            None,
                            expanded,
                            theme,
                        );
                    }
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
                            // A call and its result are one block: the result is
                            // looked up by id so it lands directly under the call
                            // it answers, and its status comes from the live list
                            // (running) or the outcome table (finished).
                            push_tool_call(
                                &mut stream,
                                call,
                                tool_results.get(call.id.as_str()).copied(),
                                outcomes.get(&call.id),
                                app.active_tool_calls.iter().any(|c| c.id == call.id),
                                expanded,
                                theme,
                            );
                        }
                    }
                    stream.raw(Line::raw(""));
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                    name,
                } => {
                    // Results already claimed by their call are painted there;
                    // only orphans (compacted or resumed transcripts) stand alone.
                    if referenced_tools.contains(tool_call_id.as_str()) {
                        continue;
                    }
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

                    push_tool_output(&mut stream, content, expanded, theme);
                    stream.raw(Line::raw(""));
                }
            }
        }

        // 2. The in-flight turn, in the exact order its deltas arrived. The
        //    transcript is one chronological stream, so thinking and the answer
        //    interleave the way they were produced instead of being regrouped
        //    into one lane above the other.
        let mut reasoning_at = 0usize;
        let mut text_at = 0usize;
        let mut answer_started = false;
        for (kind, bytes) in app.live_segments.clone() {
            match kind {
                LiveKind::Reasoning => {
                    let Some(slice) = app.reasoning_delta.get(reasoning_at..reasoning_at + bytes)
                    else {
                        continue;
                    };
                    reasoning_at += bytes;
                    push_thinking(
                        &mut stream,
                        &mut ctx,
                        markdown_on,
                        slice,
                        Some(spinner),
                        expanded,
                        theme,
                    );
                }
                LiveKind::Text => {
                    let Some(slice) = app.streaming_delta.get(text_at..text_at + bytes) else {
                        continue;
                    };
                    text_at += bytes;
                    if !answer_started {
                        stream.raw(Line::from(vec![Span::styled(
                            "⚡ Thunder Assistant",
                            Style::default()
                                .fg(theme.assistant_bubble)
                                .add_modifier(Modifier::BOLD),
                        )]));
                        answer_started = true;
                    }
                    push_body(&mut stream, &mut ctx, markdown_on, slice, raw_style);
                }
            }
        }
        if answer_started {
            stream.raw(Line::raw(""));
        }

        // Nothing has streamed yet: the model is still opening its turn, so the
        // thinking header stands in for the block that is about to fill in.
        if app.live_segments.is_empty()
            && app.reasoning_delta.is_empty()
            && app.streaming_delta.is_empty()
            && app.agent_status == AgentStatus::Thinking
        {
            push_thinking(
                &mut stream,
                &mut ctx,
                markdown_on,
                "",
                Some(spinner),
                expanded,
                theme,
            );
        }
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

    let total_rows = row_end.last().copied().unwrap_or(0);
    TranscriptCache {
        key,
        lines: stream.lines,
        row_end,
        links,
        marks,
        total_rows,
    }
}

/// Paints a cached transcript: the scroll bound, the click hitboxes and the
/// rail marks all come from the cache, and only the visible slice of lines is
/// handed to the pane — rows above the viewport are skipped rather than wrapped
/// and thrown away.
fn paint_transcript(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    theme: &Theme,
    cache: &TranscriptCache,
) {
    let row_end = &cache.row_end;
    let links = &cache.links;
    let total_rendered_lines = cache.total_rows;

    let visible_height = area.height.saturating_sub(2) as usize;
    let max_scroll = total_rendered_lines.saturating_sub(visible_height);
    app.last_max_scroll = max_scroll;

    let scroll_y = if app.auto_scroll {
        max_scroll
    } else {
        app.scroll_offset.min(max_scroll)
    };

    app.timeline_marks = cache.marks.clone();

    // Project link hitboxes into screen coordinates for this frame's viewport.
    app.link_hitboxes = if app.markdown_enabled {
        links
            .iter()
            .filter_map(|link| {
                let first_row = rows_before(row_end, link.line);
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

    // The busy indicator sits at the start of the separator directly above the
    // prompt, so it is always in view without competing with what is typed.
    if app.is_running() {
        let label = if app.agent_status == AgentStatus::Stopping {
            "stopping…"
        } else {
            "working…"
        };
        block = block.title_bottom(
            Line::from(Span::styled(
                format!("{} {label}", app.spinner()),
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::ITALIC),
            ))
            .alignment(Alignment::Left),
        );
    }

    // `Paragraph::scroll` takes a `u16`; a transcript taller than that cannot be
    // addressed exactly, so clamp instead of wrapping around to the top.
    let (first_line, end_line) = visible_line_range(cache, scroll_y, visible_height);
    let inner_scroll = scroll_y.saturating_sub(rows_before(row_end, first_line));
    let lines: Vec<Line<'static>> = cache.lines[first_line..end_line].to_vec();
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });

    f.render_widget(
        paragraph
            .block(block)
            .scroll((inner_scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
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

/// Truncates to `max` characters, appending an ellipsis when anything was cut.
fn truncate_to(text: &str, max: usize) -> String {
    let mut out = thunder_agent_providers::naming::truncate_chars(text, max.max(1));
    if text.chars().count() > max.max(1) {
        out.push('…');
    }
    out
}

/// First non-empty line of a thinking block, short enough to stay one row.
fn thinking_excerpt(text: &str, width: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let budget = width.saturating_sub(30).max(16);
    truncate_to(line, budget)
}

/// One thinking block: a one-line summary by default, the full text with
/// `Ctrl + O`. `spinner` is only set while the model is still thinking.
fn push_thinking(
    stream: &mut Stream,
    ctx: &mut RenderCtx<'_>,
    markdown_on: bool,
    text: &str,
    spinner: Option<&str>,
    expanded: bool,
    theme: &Theme,
) {
    let lines = text.lines().filter(|line| !line.trim().is_empty()).count();
    let mut spans = vec![Span::styled(
        "🧠 Thinking",
        Style::default()
            .fg(theme.tool_bubble)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(frame) = spinner {
        spans.push(Span::styled(
            format!(" {frame}"),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::ITALIC),
        ));
    }
    if lines > 0 {
        let noun = if lines == 1 { "line" } else { "lines" };
        spans.push(Span::styled(
            format!(" · {lines} {noun}"),
            theme.muted_style(),
        ));
    }
    if !expanded {
        let excerpt = thinking_excerpt(text, stream.width as usize);
        if !excerpt.is_empty() {
            spans.push(Span::styled(" · ", theme.muted_style()));
            spans.push(Span::styled(
                excerpt,
                Style::default()
                    .fg(theme.text_muted)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
    }
    stream.raw(Line::from(spans));

    if expanded && !text.trim().is_empty() {
        if markdown_on {
            stream.append(markdown::render(text, ctx));
        } else {
            for line in text.lines() {
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
    }
    stream.raw(Line::raw(""));
}

/// One tool call and the result that answers it, as a single block.
fn push_tool_call(
    stream: &mut Stream,
    call: &ToolCall,
    output: Option<&str>,
    outcome: Option<&ToolOutcome>,
    running: bool,
    expanded: bool,
    theme: &Theme,
) {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(
            format!("⚙ {}", call.function.name),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let (status, status_color) = if running {
        ("⏳ running".to_string(), theme.tool_bubble)
    } else if let Some(outcome) = outcome {
        if outcome.is_error {
            ("✖ Failed".to_string(), theme.error_color)
        } else {
            (
                format!("✔ {}", format_duration(outcome.duration_ms)),
                theme.assistant_bubble,
            )
        }
    } else {
        (String::new(), theme.text_muted)
    };
    if !status.is_empty() {
        spans.push(Span::styled(" · ", theme.muted_style()));
        spans.push(Span::styled(status, Style::default().fg(status_color)));
    }
    if let Some(output) = output {
        let lines = output.lines().count();
        if lines > 0 {
            let noun = if lines == 1 { "line" } else { "lines" };
            spans.push(Span::styled(
                format!(" · {lines} {noun}"),
                theme.muted_style(),
            ));
        }
    }
    stream.raw(Line::from(spans));

    if expanded && !call.function.arguments.trim().is_empty() {
        stream.raw(Line::from(vec![
            Span::raw("    "),
            Span::styled("args: ", theme.muted_style()),
            Span::styled(
                truncate_to(&call.function.arguments, TOOL_ARGS_CHARS),
                Style::default().fg(theme.text_muted),
            ),
        ]));
    }
    if let Some(output) = output {
        push_tool_output(stream, output, expanded, theme);
    }
}

/// A tool's output: at most [`TOOL_PREVIEW_LINES`] while collapsed, the whole
/// thing (up to [`TOOL_EXPANDED_MAX_LINES`]) with `Ctrl + O`.
fn push_tool_output(stream: &mut Stream, output: &str, expanded: bool, theme: &Theme) {
    let budget = (stream.width as usize).saturating_sub(8).max(16);
    if expanded {
        for (idx, line) in output.lines().enumerate() {
            if idx >= TOOL_EXPANDED_MAX_LINES {
                let rest = output.lines().count() - TOOL_EXPANDED_MAX_LINES;
                stream.raw(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        format!("... ({rest} more lines — Ctrl+O to collapse)"),
                        theme.muted_style(),
                    ),
                ]));
                break;
            }
            stream.raw(Line::from(vec![
                Span::raw("    "),
                Span::styled(line.to_string(), Style::default().fg(TOOL_OUTPUT_COLOR)),
            ]));
        }
    } else {
        for line in output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(TOOL_PREVIEW_LINES)
        {
            stream.raw(Line::from(vec![
                Span::raw("    └ "),
                Span::styled(
                    truncate_to(line, budget),
                    Style::default().fg(TOOL_OUTPUT_COLOR),
                ),
            ]));
        }
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

    /// The cheap measure (a line that fits the pane is one row) must agree with
    /// the wrapper for every single line, not just in aggregate: a line the
    /// fast path calls one row while the pane paints two would put every
    /// hitbox below it a row off.
    #[test]
    fn the_cheap_row_measure_agrees_with_the_wrapper_line_by_line() {
        let corpus: Vec<Line<'static>> = vec![
            Line::raw(""),
            Line::raw(" "),
            Line::raw("   "),
            Line::raw(" ".repeat(79)),
            Line::raw(" ".repeat(80)),
            Line::raw("\t"),
            Line::raw("\u{200b}".repeat(4)),
            Line::raw("\u{a0}".repeat(3)),
            Line::raw(" \u{200b} "),
            Line::raw("short"),
            Line::raw("a\u{200b}b"),
            Line::raw("a".repeat(80)),
            Line::raw("a".repeat(81)),
            Line::raw("a".repeat(79) + " "),
            Line::raw(format!("ab{}", " ".repeat(78))),
            Line::raw("中".repeat(40)),
            Line::raw("中".repeat(41)),
            Line::raw("a".repeat(78) + "中"),
            Line::raw("a".repeat(79) + "中"),
            Line::raw("👍".repeat(40)),
            Line::raw("👍".repeat(41)),
            Line::raw("e\u{301}e\u{301}".repeat(40)),
            Line::raw("🇨🇳".repeat(40)),
            Line::raw("👨‍👩‍👧‍👦 ".repeat(20)),
            Line::raw("wrapped words ".repeat(30)),
            Line::raw(format!("{}   ", "t".repeat(78))),
            Line::from(vec![Span::raw("x".repeat(40)), Span::raw("y".repeat(40))]),
            Line::from(vec![Span::raw("x".repeat(40)), Span::raw("y".repeat(41))]),
            Line::from(vec![Span::raw("   "), Span::raw("中".repeat(39))]),
        ];

        for width in [1u16, 7, 20, 80] {
            for line in &corpus {
                let painted = Paragraph::new(vec![line.clone()])
                    .wrap(Wrap { trim: false })
                    .line_count(width);
                assert_eq!(
                    Stream::rows(line, width),
                    painted,
                    "width {width}, line {line:?}"
                );
            }
        }
    }
}
