use crate::app::{App, TimelineItem, ToolCardStatus};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(4),
        ])
        .split(frame.area());

    // 1. Top Header Bar
    let snap = app.tool_registry.current();
    let mode_color = if app.mode == "plan" {
        Color::Yellow
    } else {
        Color::Green
    };
    let header_line = Line::from(vec![
        Span::styled(
            " ⚡ Thunder ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("│ Model: "),
        Span::styled(&app.model, Style::default().fg(Color::White)),
        Span::raw(" │ Mode: "),
        Span::styled(
            format!("[{}]", app.mode.to_uppercase()),
            Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " │ Catalog v{} ({} tools) │ {}",
            snap.version,
            snap.descriptors.len(),
            app.workspace_dir.display()
        )),
    ]);
    let header = Paragraph::new(header_line).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Microkernel Session "),
    );
    frame.render_widget(header, chunks[0]);

    // 2. Center Timeline
    let mut lines: Vec<Line> = Vec::new();
    for item in &app.timeline {
        match item {
            TimelineItem::User { text } => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "❯ You: ",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
                lines.push(Line::from(""));
            }
            TimelineItem::Assistant {
                content,
                reasoning,
                streaming,
            } => {
                if !reasoning.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("  💭 Thinking: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(reasoning, Style::default().fg(Color::DarkGray)),
                    ]));
                }
                let prefix = if *streaming {
                    "⚡ Thunder (streaming): "
                } else {
                    "⚡ Thunder: "
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(content),
                ]));
                lines.push(Line::from(""));
            }
            TimelineItem::ToolCallCard {
                name,
                args_summary,
                status,
                ..
            } => {
                let (badge, color, detail) = match status {
                    ToolCardStatus::Running => ("⏳ RUN", Color::Yellow, args_summary.clone()),
                    ToolCardStatus::Success {
                        output_preview,
                        duration_ms,
                    } => (
                        "✓ OK",
                        Color::Green,
                        format!("({duration_ms}ms) {output_preview}"),
                    ),
                    ToolCardStatus::Error {
                        error_preview,
                        duration_ms,
                    } => (
                        "✖ ERR",
                        Color::Red,
                        format!("({duration_ms}ms) {error_preview}"),
                    ),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  [{badge}] {name} "),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(detail, Style::default().fg(Color::Gray)),
                ]));
            }
            TimelineItem::SystemNotice { text } => {
                lines.push(Line::from(vec![
                    Span::styled("  ℹ ", Style::default().fg(Color::Blue)),
                    Span::styled(text, Style::default().fg(Color::Gray)),
                ]));
            }
        }
    }

    let timeline_widget = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Timeline "))
        .wrap(Wrap { trim: false })
        .scroll((app.scroll_offset, 0));
    frame.render_widget(timeline_widget, chunks[1]);

    // 3. Bottom Composer + Status Bar
    let input_title = if app.is_running {
        format!(
            " Steer Active Run (Enter to inject) │ {} ",
            app.status_message
        )
    } else {
        format!(
            " Prompt (~{} tokens) │ {} │ Enter: Send · Shift+Tab: Mode · Ctrl+P: Pause · /help ",
            app.estimated_tokens, app.status_message
        )
    };
    let composer = Paragraph::new(app.input.as_str())
        .block(Block::default().borders(Borders::ALL).title(input_title));
    frame.render_widget(composer, chunks[2]);
}
