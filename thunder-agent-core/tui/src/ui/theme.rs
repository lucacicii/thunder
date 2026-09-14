use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub surface: Color,
    pub border_normal: Color,
    pub border_focus: Color,
    pub accent_primary: Color,
    pub accent_secondary: Color,
    pub user_bubble: Color,
    pub assistant_bubble: Color,
    pub tool_bubble: Color,
    pub error_color: Color,
    pub text_main: Color,
    pub text_muted: Color,
    pub highlight: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Rgb(13, 17, 23),
            surface: Color::Rgb(22, 27, 34),
            border_normal: Color::Rgb(48, 54, 61),
            border_focus: Color::Rgb(56, 189, 248), // Electric Cyan
            accent_primary: Color::Rgb(56, 189, 248),
            accent_secondary: Color::Rgb(168, 85, 247), // Purple
            user_bubble: Color::Rgb(96, 165, 250),     // Blue
            assistant_bubble: Color::Rgb(52, 211, 153), // Emerald Green
            tool_bubble: Color::Rgb(251, 191, 36),     // Amber
            error_color: Color::Rgb(248, 113, 113),    // Red
            text_main: Color::Rgb(240, 246, 252),
            text_muted: Color::Rgb(139, 148, 158),
            highlight: Color::Rgb(255, 215, 0),
        }
    }
}

impl Theme {
    pub fn title_style(&self) -> Style {
        Style::default()
            .fg(self.accent_primary)
            .add_modifier(Modifier::BOLD)
    }

    pub fn focus_border(&self, is_focused: bool) -> Style {
        if is_focused {
            Style::default()
                .fg(self.border_focus)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.border_normal)
        }
    }

    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text_main)
    }

    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.text_muted)
    }

    pub fn error_style(&self) -> Style {
        Style::default()
            .fg(self.error_color)
            .add_modifier(Modifier::BOLD)
    }
}
