use ratatui::style::{Color, Modifier, Style};

/// Complete aesthetic theme tokens for terminal Markdown rendering.
///
/// Designed with high contrast, modern developer aesthetic (Geist / Tokyo Night inspired).
#[derive(Debug, Clone)]
pub struct MarkdownTheme {
    pub h1: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4_to_h6: Style,
    pub text: Style,
    pub bold: Style,
    pub italic: Style,
    pub strikethrough: Style,
    pub inline_code: Style,
    pub code_header: Style,
    pub code_badge: Style,
    pub code_border: Style,
    pub code_bg: Color,
    pub code_text: Style,
    pub code_line_number: Style,
    pub code_keyword: Style,
    pub code_string: Style,
    pub code_number: Style,
    pub code_comment: Style,
    pub code_type: Style,
    pub quote_border: Style,
    pub quote_text: Style,
    pub callout_tip: Style,
    pub callout_note: Style,
    pub callout_warning: Style,
    pub callout_important: Style,
    pub list_bullet: Style,
    pub checkbox_checked: Style,
    pub checkbox_unchecked: Style,
    pub table_border: Style,
    pub table_header: Style,
    pub rule: Style,
    pub link: Style,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            h1: Style::default()
                .fg(Color::Rgb(56, 189, 248)) // Sky blue
                .add_modifier(Modifier::BOLD),
            h2: Style::default()
                .fg(Color::Rgb(248, 250, 252)) // Bright slate
                .add_modifier(Modifier::BOLD),
            h3: Style::default()
                .fg(Color::Rgb(148, 163, 184)) // Soft slate
                .add_modifier(Modifier::BOLD),
            h4_to_h6: Style::default()
                .fg(Color::Rgb(148, 163, 184))
                .add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::Rgb(226, 232, 240)),
            bold: Style::default()
                .fg(Color::Rgb(255, 255, 255))
                .add_modifier(Modifier::BOLD),
            italic: Style::default()
                .fg(Color::Rgb(203, 213, 225))
                .add_modifier(Modifier::ITALIC),
            strikethrough: Style::default()
                .fg(Color::Rgb(100, 116, 139))
                .add_modifier(Modifier::CROSSED_OUT),
            inline_code: Style::default()
                .fg(Color::Rgb(125, 211, 252))
                .bg(Color::Rgb(30, 41, 59)),
            code_header: Style::default().fg(Color::Rgb(148, 163, 184)),
            code_badge: Style::default()
                .fg(Color::Rgb(250, 204, 21)) // Amber
                .add_modifier(Modifier::BOLD),
            code_border: Style::default().fg(Color::Rgb(51, 65, 85)),
            code_bg: Color::Rgb(15, 23, 42),
            code_text: Style::default().fg(Color::Rgb(226, 232, 240)),
            code_line_number: Style::default().fg(Color::Rgb(71, 85, 105)),
            code_keyword: Style::default()
                .fg(Color::Rgb(192, 132, 252)) // Purple
                .add_modifier(Modifier::BOLD),
            code_string: Style::default().fg(Color::Rgb(74, 222, 128)), // Emerald
            code_number: Style::default().fg(Color::Rgb(251, 146, 60)), // Orange
            code_comment: Style::default()
                .fg(Color::Rgb(100, 116, 139))
                .add_modifier(Modifier::ITALIC),
            code_type: Style::default()
                .fg(Color::Rgb(56, 189, 248)) // Cyan
                .add_modifier(Modifier::BOLD),
            quote_border: Style::default().fg(Color::Rgb(245, 158, 11)), // Amber bar
            quote_text: Style::default()
                .fg(Color::Rgb(203, 213, 225))
                .add_modifier(Modifier::ITALIC),
            callout_tip: Style::default()
                .fg(Color::Rgb(34, 197, 94))
                .add_modifier(Modifier::BOLD),
            callout_note: Style::default()
                .fg(Color::Rgb(56, 189, 248))
                .add_modifier(Modifier::BOLD),
            callout_warning: Style::default()
                .fg(Color::Rgb(234, 179, 8))
                .add_modifier(Modifier::BOLD),
            callout_important: Style::default()
                .fg(Color::Rgb(239, 68, 68))
                .add_modifier(Modifier::BOLD),
            list_bullet: Style::default().fg(Color::Rgb(56, 189, 248)),
            checkbox_checked: Style::default()
                .fg(Color::Rgb(34, 197, 94))
                .add_modifier(Modifier::BOLD),
            checkbox_unchecked: Style::default().fg(Color::Rgb(100, 116, 139)),
            table_border: Style::default().fg(Color::Rgb(71, 85, 105)),
            table_header: Style::default()
                .fg(Color::Rgb(248, 250, 252))
                .add_modifier(Modifier::BOLD),
            rule: Style::default().fg(Color::Rgb(51, 65, 85)),
            link: Style::default()
                .fg(Color::Rgb(56, 189, 248))
                .add_modifier(Modifier::UNDERLINED),
        }
    }
}
