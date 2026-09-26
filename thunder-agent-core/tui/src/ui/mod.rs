pub mod chat;
pub mod command_popup;
pub mod header;
pub mod help;
pub mod picker_modal;
pub mod sidebar;
pub mod status_bar;
pub mod theme;

use crate::app::{App, ViewMode};
use crate::ui::chat::render_chat;
use crate::ui::command_popup::render_command_popup;
use crate::ui::header::render_header;
use crate::ui::help::render_help_modal;
use crate::ui::picker_modal::render_picker_modal;
use crate::ui::status_bar::{render_input, render_status_bar};
use crate::ui::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // Header status bar
            Constraint::Min(4),     // Full-width dialogue stream
            Constraint::Length(1),  // Claude Code-style inline prompt line (❯ ...)
            Constraint::Length(1),  // Minimal status footer
        ])
        .split(f.area());

    // 1. Render Header (Top status line)
    render_header(f, app, main_chunks[0], theme);

    // 2. Render Main Dialogue Stream (Full screen width, no sidebar)
    match app.mode {
        _ => {
            render_chat(f, app, main_chunks[1], theme);
        }
    }

    // 3. Render Claude Code-style interactive inline prompt line
    render_input(f, app, main_chunks[2], theme);

    // 4. Render Slash Command Autocomplete Popover (floating above the prompt line)
    render_command_popup(f, app, main_chunks[2], theme);

    // 5. Render Minimal Status Footer
    render_status_bar(f, app, main_chunks[3], theme);

    // 6. Render Interactive Picker Dropdown Modal if active (top layer)
    if app.picker.is_open {
        render_picker_modal(f, app, theme);
    }

    // 7. Render Help Modal Overlay if active
    if app.mode == ViewMode::Help {
        render_help_modal(f, f.area(), theme);
    }
}
