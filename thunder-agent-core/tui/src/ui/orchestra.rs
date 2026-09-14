use crate::app::App;
use crate::ui::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

pub fn render_orchestra_monitor(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),  // Topology pipeline banner
            Constraint::Min(8),     // Stages and agent records
        ])
        .split(area);

    // 1. Pipeline Topology Banner
    let banner_block = Block::default()
        .borders(Borders::ALL)
        .title(" 🎼 Multi-Agent Orchestra Topology ")
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.border_focus));

    let topology_name = app
        .conversation
        .orchestration
        .as_ref()
        .and_then(|o| o.topology.as_deref())
        .unwrap_or("Sequential Pipeline");

    let pipeline_diagram = match topology_name.to_lowercase().as_str() {
        "parallel" => {
            vec![
                Line::from(vec![
                    Span::styled("  Topology: ", theme.muted_style()),
                    Span::styled("PARALLEL MULTI-AGENT", Style::default().fg(theme.accent_secondary).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(vec![
                    Span::styled("  ├── [Planner Agent]  ", Style::default().fg(theme.accent_primary)),
                    Span::styled("──► Parallel Execution", theme.muted_style()),
                ]),
                Line::from(vec![
                    Span::styled("  └── [Reviewer Agent] ", Style::default().fg(theme.tool_bubble)),
                    Span::styled("──► Parallel Execution", theme.muted_style()),
                ]),
            ]
        }
        _ => {
            vec![
                Line::from(vec![
                    Span::styled("  Topology: ", theme.muted_style()),
                    Span::styled("SEQUENTIAL PIPELINE", Style::default().fg(theme.accent_primary).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(vec![
                    Span::styled("  [1. Planner] ", Style::default().fg(theme.accent_primary).add_modifier(Modifier::BOLD)),
                    Span::styled("──(final_content)──► ", Style::default().fg(theme.border_normal)),
                    Span::styled("[2. Coder] ", Style::default().fg(theme.assistant_bubble).add_modifier(Modifier::BOLD)),
                    Span::styled("──(implementation)──► ", Style::default().fg(theme.border_normal)),
                    Span::styled("[3. Verify]", Style::default().fg(theme.tool_bubble)),
                ]),
            ]
        }
    };

    let banner_para = Paragraph::new(pipeline_diagram).block(banner_block);
    f.render_widget(banner_para, chunks[0]);

    // 2. Stage Execution Records
    let mut stage_lines: Vec<Line> = Vec::new();

    if app.conversation.stages.is_empty() {
        stage_lines.push(Line::styled(
            " No multi-agent stage records in this session yet.",
            theme.muted_style(),
        ));
        stage_lines.push(Line::styled(
            " When running through Orchestra, stages and handoff briefs will appear here.",
            theme.muted_style(),
        ));
    } else {
        for (i, stage) in app.conversation.stages.iter().enumerate() {
            stage_lines.push(Line::from(vec![
                Span::styled(format!(" ▶ Stage {}: ", i + 1), Style::default().fg(theme.accent_primary).add_modifier(Modifier::BOLD)),
                Span::styled(format!("Role: `{}` | Agent: `{}`", stage.role, stage.agent_id), Style::default().fg(theme.text_main).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" ({}ms, {} turns, {} tools)", stage.duration_ms, stage.turn_count, stage.tool_calls_count), theme.muted_style()),
            ]));

            stage_lines.push(Line::from(vec![
                Span::styled("   Brief: ", Style::default().fg(theme.tool_bubble)),
                Span::styled(&stage.task_brief, Style::default().fg(theme.text_muted)),
            ]));

            if let Some(final_text) = &stage.final_content {
                stage_lines.push(Line::from(vec![
                    Span::styled("   Outcome: ", Style::default().fg(theme.assistant_bubble)),
                ]));
                for l in final_text.lines().take(4) {
                    stage_lines.push(Line::from(vec![
                        Span::raw("     "),
                        Span::styled(l, Style::default().fg(Color::Rgb(200, 210, 225))),
                    ]));
                }
            }

            stage_lines.push(Line::raw(""));
        }
    }

    let stages_block = Block::default()
        .borders(Borders::ALL)
        .title(" 📋 Recorded Execution Stages ")
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.border_normal));

    let stages_para = Paragraph::new(stage_lines)
        .block(stages_block)
        .wrap(Wrap { trim: false });

    f.render_widget(stages_para, chunks[1]);
}
