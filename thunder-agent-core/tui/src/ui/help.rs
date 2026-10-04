use crate::ui::theme::Theme;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

pub fn render_help_modal(f: &mut Frame, area: Rect, theme: &Theme) {
    let help_area = centered_rect(75, 75, area);

    let shortcuts = vec![
        Line::styled(
            "⚡ THUNDER TUI — INTERACTIVE COMMANDS & SHORTCUTS",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::styled(
            "⌨️  KEYBOARD SHORTCUTS",
            Style::default()
                .fg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(
                "Enter          ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Send message / Execute prompt or slash command",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Tab            ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Autocomplete slash command or its argument / Switch focus pane",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + N       ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Start new conversation session", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + P       ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Cycle mode: Auto ➔ Single", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + B       ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Toggle sidebar visibility", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + H       ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Toggle this help window", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled(
                "/metrics [on|off]     ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Toggle the token, cache and speed bar above the prompt",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/queue [clear]        ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Inspect or clear messages queued into the running agent",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + O       ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Every link and file path in this session (Enter reveals/opens)",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Up / Down      ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Select command suggestion / History / Scroll chat",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Cmd + ← / →    ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Jump the caret to the start / end of the prompt (Home/End, Ctrl+A/E)",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Click          ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "On a link or file path: open it, or reveal it in Finder (/links lists them)",
                theme.text_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Ctrl + C / Esc ",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Cancel running agent or dismiss popup", theme.text_style()),
        ]),
        Line::from(vec![
            Span::styled(
                "/pause /unpause",
                Style::default()
                    .fg(theme.highlight)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Hold the running agent at the next tool boundary, then release it",
                theme.text_style(),
            ),
        ]),
        Line::raw(""),
        Line::styled(
            "⚡ SLASH COMMANDS (type / in input box for autocompletion):",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(vec![
            Span::styled(
                "/help                     ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Show full command and shortcut reference",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/model [name]             ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "View or switch active LLM model (gpt-4o, claude-3-7, etc.)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/mode [auto|single]       ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Switch execution mode (plugin host or direct single agent)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/think [off|low|med|high] ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Set the reasoning effort for this conversation",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/permission [read|write|…]",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Set the tool capability tier (read / write / bash)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/roots [add|remove|clear] ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Grant extra workspace roots for multi-repo tasks",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/ask [on|off]             ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Let the agent ask clarifying questions (terminal modal)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/trace [list|<task_id>]   ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Inspect execution traces recorded for this session",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/title [text|force]       ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Set the conversation title manually or (re)generate it",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/skills [list|load|scan]  ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Browse skills, inspect playbooks, or re-scan dirs",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/mcp [list|servers|reload]",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "List connected MCP servers & discovered remote tools",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/config [key] [val]       ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "View or update runtime settings (temp, turns, timeout)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/compact                  ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Compact dialogue history to optimize tokens",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/stats                    ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Show dialogue turns, tool executions & token estimation",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/workspace [path]         ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Display or switch active workspace directory",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/export [path]            ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Export current conversation to a Markdown file",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/preview [on|off]         ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Toggle rendered Markdown preview against the raw source",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/links                    ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "List links & file paths in this session (Enter reveals/opens)",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/clear                    ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Reset conversation and start fresh session",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/health                   ",
                Style::default().fg(theme.assistant_bubble),
            ),
            Span::styled(
                "Run local agent and environment diagnostics",
                theme.muted_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "/quit                     ",
                Style::default().fg(theme.error_color),
            ),
            Span::styled("Exit Thunder TUI", theme.muted_style()),
        ]),
        Line::raw(""),
        Line::styled(
            "Press Esc or Ctrl+H to close this window",
            theme.muted_style(),
        ),
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
