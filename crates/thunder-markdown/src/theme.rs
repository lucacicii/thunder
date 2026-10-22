use ratatui::style::{Color, Modifier, Style};
use tui_markdown::{AlertKind, StyleSheet};

/// Thunder's aesthetic theme tokens mapped onto the `tui-markdown` style sheet.
///
/// The heavy lifting (parsing, tables, alerts, code highlighting) is delegated to
/// the `tui-markdown` crate; this struct only supplies our palette.
#[derive(Debug, Clone)]
pub struct MarkdownTheme {
    pub h1: Style,
    pub h2: Style,
    pub h3: Style,
    pub h4_to_h6: Style,
    pub code: Style,
    pub link: Style,
    pub blockquote: Style,
    pub alert_note: Style,
    pub alert_tip: Style,
    pub alert_important: Style,
    pub alert_warning: Style,
    pub alert_caution: Style,
    pub table_header: Style,
    pub table_cell: Style,
    pub table_border: Style,
    pub list_marker: Style,
    pub html: Style,
    pub metadata_block: Style,
    pub image_alt: Style,
}

impl Default for MarkdownTheme {
    fn default() -> Self {
        Self {
            h1: Style::default()
                .fg(Color::Rgb(56, 189, 248))
                .add_modifier(Modifier::BOLD),
            h2: Style::default()
                .fg(Color::Rgb(248, 250, 252))
                .add_modifier(Modifier::BOLD),
            h3: Style::default()
                .fg(Color::Rgb(148, 163, 184))
                .add_modifier(Modifier::BOLD),
            h4_to_h6: Style::default()
                .fg(Color::Rgb(148, 163, 184))
                .add_modifier(Modifier::BOLD),
            code: Style::default()
                .fg(Color::Rgb(125, 211, 252))
                .bg(Color::Rgb(30, 41, 59)),
            link: Style::default()
                .fg(Color::Rgb(56, 189, 248))
                .add_modifier(Modifier::UNDERLINED),
            blockquote: Style::default()
                .fg(Color::Rgb(203, 213, 225))
                .add_modifier(Modifier::ITALIC),
            alert_note: Style::default().fg(Color::Rgb(56, 189, 248)),
            alert_tip: Style::default().fg(Color::Rgb(34, 197, 94)),
            alert_important: Style::default().fg(Color::Rgb(192, 132, 252)),
            alert_warning: Style::default().fg(Color::Rgb(234, 179, 8)),
            alert_caution: Style::default().fg(Color::Rgb(239, 68, 68)),
            table_header: Style::default()
                .fg(Color::Rgb(248, 250, 252))
                .add_modifier(Modifier::BOLD),
            table_cell: Style::default().fg(Color::Rgb(226, 232, 240)),
            table_border: Style::default().fg(Color::Rgb(71, 85, 105)),
            list_marker: Style::default().fg(Color::Rgb(56, 189, 248)),
            html: Style::default().fg(Color::Rgb(100, 116, 139)),
            metadata_block: Style::default().fg(Color::Rgb(250, 204, 21)),
            image_alt: Style::default()
                .fg(Color::Rgb(100, 116, 139))
                .add_modifier(Modifier::ITALIC),
        }
    }
}

impl StyleSheet for MarkdownTheme {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => self.h1,
            2 => self.h2,
            3 => self.h3,
            _ => self.h4_to_h6,
        }
    }

    fn code(&self) -> Style {
        self.code
    }

    fn link(&self) -> Style {
        self.link
    }

    fn blockquote(&self) -> Style {
        self.blockquote
    }

    fn html(&self) -> Style {
        self.html
    }

    fn metadata_block(&self) -> Style {
        self.metadata_block
    }

    fn alert(&self, kind: AlertKind) -> Style {
        match kind {
            AlertKind::Note => self.alert_note,
            AlertKind::Tip => self.alert_tip,
            AlertKind::Important => self.alert_important,
            AlertKind::Warning => self.alert_warning,
            AlertKind::Caution => self.alert_caution,
        }
    }

    fn table_header(&self) -> Style {
        self.table_header
    }

    fn table_cell(&self) -> Style {
        self.table_cell
    }

    fn table_border(&self) -> Style {
        self.table_border
    }

    fn list_marker(&self) -> Style {
        self.list_marker
    }

    fn image_alt(&self) -> Style {
        self.image_alt
    }
}
