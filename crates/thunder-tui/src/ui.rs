use crate::app::{
    resolve_completion_context, App, InputCompletionContext, TimelineItem, ToolCardStatus,
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
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
    let think_label = app.thinking_level.as_deref().unwrap_or("auto");
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
        Span::raw(" │ Think: "),
        Span::styled(think_label, Style::default().fg(Color::Magenta)),
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
                let mut user_spans = vec![Span::styled(
                    "❯ You: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )];
                user_spans.extend(thunder_markdown::parse_inline(text));
                lines.push(Line::from(user_spans));
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
                    "⚡ Thunder (streaming):"
                } else {
                    "⚡ Thunder:"
                };
                lines.push(Line::from(vec![Span::styled(
                    prefix,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )]));
                let md_lines =
                    thunder_markdown::render_markdown_default(content, chunks[1].width as usize);
                for md_line in md_lines {
                    lines.push(md_line);
                }
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
            " Prompt (~{} tokens) │ {} │ Enter: Send · Tab: Complete · Shift+Tab: Mode · /help ",
            app.estimated_tokens, app.status_message
        )
    };
    let composer = if app.input.is_empty() && !app.is_running {
        if let Some(sug) = app.current_suggestion() {
            let ghost_line = Line::from(vec![
                Span::styled("💡 ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    sug,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC),
                ),
                Span::styled(
                    " (Tab: Accept · ↑/↓: Switch)",
                    Style::default().fg(Color::Rgb(80, 80, 95)),
                ),
            ]);
            Paragraph::new(ghost_line)
                .block(Block::default().borders(Borders::ALL).title(input_title))
        } else {
            Paragraph::new(app.input.as_str())
                .block(Block::default().borders(Borders::ALL).title(input_title))
        }
    } else {
        Paragraph::new(app.input.as_str())
            .block(Block::default().borders(Borders::ALL).title(input_title))
    };
    frame.render_widget(composer, chunks[2]);

    // 4. Floating Command / Argument Autocomplete Popover (above bottom composer)
    if !app.picker.is_open && app.input.starts_with('/') {
        if let Some(ctx) = resolve_completion_context(&app.input, app) {
            match ctx {
                InputCompletionContext::Command { matches } => {
                    let visible_count = matches.len().min(6);
                    let pop_height = (visible_count as u16) + 2;
                    let pop_width = (chunks[2].width * 85 / 100).max(45).min(chunks[2].width);
                    let pop_x = chunks[2].x + 1;
                    let pop_y = chunks[2].y.saturating_sub(pop_height);

                    let pop_area = Rect {
                        x: pop_x,
                        y: pop_y,
                        width: pop_width,
                        height: pop_height,
                    };

                    frame.render_widget(Clear, pop_area);

                    let sel = app.autocomplete_index % matches.len();
                    let mut pop_lines = Vec::new();
                    for (idx, cmd) in matches.iter().take(visible_count).enumerate() {
                        let is_sel = idx == sel;
                        let (prefix, name_style, desc_style) = if is_sel {
                            (
                                "> ",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                                Style::default().fg(Color::White),
                            )
                        } else {
                            (
                                "  ",
                                Style::default().fg(Color::Cyan),
                                Style::default().fg(Color::DarkGray),
                            )
                        };
                        pop_lines.push(Line::from(vec![
                            Span::styled(
                                format!("{prefix}/{:<8} {:<16} ", cmd.name, cmd.args_hint),
                                name_style,
                            ),
                            Span::styled(format!("— {}", cmd.description), desc_style),
                        ]));
                    }

                    let pop_block = Block::default()
                        .borders(Borders::ALL)
                        .title(" Slash Commands (Tab: Complete · ↑/↓: Move) ")
                        .border_style(Style::default().fg(Color::Cyan))
                        .style(Style::default().bg(Color::Rgb(15, 20, 28)));

                    let pop_widget = Paragraph::new(pop_lines).block(pop_block);
                    frame.render_widget(pop_widget, pop_area);
                }
                InputCompletionContext::Argument {
                    command,
                    candidates,
                    ..
                } => {
                    let visible_count = candidates.len().min(6);
                    let pop_height = (visible_count as u16) + 2;
                    let pop_width = (chunks[2].width * 85 / 100).max(45).min(chunks[2].width);
                    let pop_x = chunks[2].x + 1;
                    let pop_y = chunks[2].y.saturating_sub(pop_height);

                    let pop_area = Rect {
                        x: pop_x,
                        y: pop_y,
                        width: pop_width,
                        height: pop_height,
                    };

                    frame.render_widget(Clear, pop_area);

                    let sel = app.autocomplete_index % candidates.len();
                    let mut pop_lines = Vec::new();
                    for (idx, cand) in candidates.iter().take(visible_count).enumerate() {
                        let is_sel = idx == sel;
                        let (prefix, style) = if is_sel {
                            (
                                "> ",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            )
                        } else {
                            ("  ", Style::default().fg(Color::Cyan))
                        };
                        pop_lines.push(Line::from(vec![Span::styled(
                            format!("{prefix}/{} {cand}", command.name),
                            style,
                        )]));
                    }

                    let pop_block = Block::default()
                        .borders(Borders::ALL)
                        .title(format!(
                            " /{} Options (Tab: Cycle · ↑/↓: Move) ",
                            command.name
                        ))
                        .border_style(Style::default().fg(Color::Yellow))
                        .style(Style::default().bg(Color::Rgb(15, 20, 28)));

                    let pop_widget = Paragraph::new(pop_lines).block(pop_block);
                    frame.render_widget(pop_widget, pop_area);
                }
            }
        }
    }

    // 5. Central Picker Modal Overlay (Full interactive dialog)
    if app.picker.is_open {
        let area = frame.area();
        let modal_width = (area.width * 75 / 100)
            .max(50)
            .min(area.width.saturating_sub(4));
        let modal_height = (area.height * 70 / 100)
            .max(12)
            .min(area.height.saturating_sub(2));

        let x = (area.width.saturating_sub(modal_width)) / 2;
        let y = (area.height.saturating_sub(modal_height)) / 2;

        let modal_area = Rect {
            x,
            y,
            width: modal_width,
            height: modal_height,
        };

        frame.render_widget(Clear, modal_area);

        let filtered = app.picker.filtered_items();
        let total = filtered.len();
        let sel_idx = if total == 0 {
            0
        } else {
            app.picker.selected_index.min(total - 1)
        };

        let title = format!(
            " {} [{}/{}] ",
            app.picker.title,
            if total == 0 { 0 } else { sel_idx + 1 },
            total
        );

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .title(title)
            .style(Style::default().bg(Color::Rgb(15, 20, 28)));

        let inner = block.inner(modal_area);
        frame.render_widget(block, modal_area);

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Search input
                Constraint::Length(1), // Divider
                Constraint::Min(4),    // Items list
                Constraint::Length(1), // Footer instructions
            ])
            .split(inner);

        // Search bar
        let search_text = if app.picker.filter_text.is_empty() {
            Span::styled(
                " (type to search...) ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(
                format!(" \"{}\" ", app.picker.filter_text),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        };
        let search_line = Line::from(vec![
            Span::styled(
                " 🔍 Search: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            search_text,
            Span::styled(
                format!(" [{} match(es)]", total),
                Style::default().fg(Color::Gray),
            ),
        ]);
        frame.render_widget(Paragraph::new(search_line), inner_chunks[0]);

        // Divider
        let divider = Line::from(Span::styled(
            "─".repeat(inner_chunks[1].width as usize),
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(divider), inner_chunks[1]);

        // Items list with scrolling
        let visible_rows = inner_chunks[2].height as usize;
        let start_idx = scroll_viewport(sel_idx, total, visible_rows);
        let end_idx = (start_idx + visible_rows).min(total);

        let mut item_lines = Vec::new();
        if total == 0 {
            item_lines.push(Line::from(vec![Span::styled(
                "  (No matching items found)",
                Style::default().fg(Color::DarkGray),
            )]));
        } else {
            for (offset, item) in filtered[start_idx..end_idx].iter().enumerate() {
                let actual_idx = start_idx + offset;
                let is_sel = actual_idx == sel_idx;
                let badge_text = item.badge.as_deref().unwrap_or("");

                if is_sel {
                    item_lines.push(Line::from(vec![
                        Span::styled(
                            " > ",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            &item.title,
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" [{badge_text}]"),
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  — {}", item.subtitle),
                            Style::default().fg(Color::Gray),
                        ),
                    ]));
                } else {
                    item_lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(&item.title, Style::default().fg(Color::Gray)),
                        Span::styled(
                            format!(" [{badge_text}]"),
                            Style::default().fg(Color::DarkGray),
                        ),
                        Span::styled(
                            format!("  — {}", item.subtitle),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            }
        }
        frame.render_widget(Paragraph::new(item_lines), inner_chunks[2]);

        // Footer instructions
        let footer_line = Line::from(vec![Span::styled(
            " [↑/↓] Move │ [Enter] Select │ [Esc] Cancel │ Type to filter ",
            Style::default().fg(Color::DarkGray),
        )]);
        frame.render_widget(Paragraph::new(footer_line), inner_chunks[3]);
    }
}

fn scroll_viewport(selected: usize, total: usize, visible: usize) -> usize {
    if total <= visible || visible == 0 {
        return 0;
    }
    let half = visible / 2;
    if selected <= half {
        0
    } else if selected + (visible - half) >= total {
        total.saturating_sub(visible)
    } else {
        selected - half
    }
}
