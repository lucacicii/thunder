use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn render_help_modal(f: &mut Frame, area: Rect, theme: &Theme) {
    let help_area = centered_rect(75, 75, area);

    let shortcuts = vec![
        Line::styled("⚡ THUNDER TUI — INTERACTIVE COMMANDS & SHORTCUTS", Style::default().fg(theme.accent_primary).add_modifier(Modifier::BOLD)),
        Line::raw(""),
        Line::styled("⌨️  KEYBOARD SHORTCUTS", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled("Enter          ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Send message / Execute prompt or slash command", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Tab            ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Autocomplete slash command / Switch focus pane", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + N       ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Start new conversation session", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + P       ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Cycle mode: Auto ➔ Pipeline ➔ Parallel ➔ Single", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + B       ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Toggle sidebar visibility", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + M       ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Toggle Orchestra multi-agent monitor", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + H       ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Toggle this help window", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Up / Down      ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Select command suggestion / History / Scroll chat", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled("Ctrl + C / Esc ", Style::default().fg(theme.highlight).add_modifier(Modifier::BOLD)),
            Span::styled("Cancel running agent or dismiss popup", theme.text_style()),
        ]),
        Line::raw(""),
        Line::styled("⚡ SLASH COMMANDS (type / in input box for autocompletion):", Style::default().fg(theme.accent_primary).add_modifier(Modifier::BOLD)),
        Line::from(vec![
            Span::styled("/help                     ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Show full command and shortcut reference", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/model [name]             ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("View or switch active LLM model (gpt-4o, claude-3-7, etc.)", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/mode [auto|pipe|par|dir] ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Switch orchestration topology mode", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/skills [list|load|scan]  ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Browse skills, inspect playbooks, or re-scan dirs", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/mcp [list|servers|reload]", Style::default().fg(theme.assistant_bubble)),
            Span::styled("List connected MCP servers & discovered remote tools", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/config [key] [val]       ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("View or update runtime settings (temp, turns, timeout)", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/compact                  ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Compact dialogue history to optimize tokens", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/stats                    ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Show dialogue turns, tool executions & token estimation", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/workspace [path]         ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Display or switch active workspace directory", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/export [path]            ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Export current conversation to a Markdown file", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/clear                    ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Reset conversation and start fresh session", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/health                   ", Style::default().fg(theme.assistant_bubble)),
            Span::styled("Run Orchestra system diagnostic probe", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/pipeline <task>          ", Style::default().fg(theme.accent_secondary)),
            Span::styled("Directly run Sequential Pipeline (Planner ➔ Coder)", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/parallel <task>          ", Style::default().fg(theme.accent_secondary)),
            Span::styled("Directly run Parallel Council (Planner + Reviewer)", theme.muted_style()),
        ]),
        Line::from(vec![
            Span::styled("/quit                     ", Style::default().fg(theme.error_color)),
            Span::styled("Exit Thunder TUI", theme.muted_style()),
        ]),
        Line::raw(""),
        Line::styled("Press Esc or Ctrl+H to close this window", theme.muted_style()),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 📖 Help & Command Reference ")
        .title_style(theme.title_style())
        .border_style(Style::default().fg(theme.border_focus));

    let paragraph = Paragraph::new(shortcuts)
        .block(block)
        .alignment(Alignment::Left);

    f.render_widget(Clear, help_area);
    f.render_widget(paragraph, help_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let popup_height = r.height * percent_y / 100;
    let x = (r.width.saturating_sub(popup_width)) / 2;
    let y = (r.height.saturating_sub(popup_height)) / 2;
    Rect::new(x, y, popup_width, popup_height)
}
