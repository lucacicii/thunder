pub mod chat;
pub mod command_popup;
pub mod header;
pub mod help;
pub mod markdown;
pub mod metrics;
pub mod picker_modal;
pub mod question_modal;
pub mod sidebar;
pub mod status_bar;
pub mod theme;

use crate::app::{App, ViewMode};
use crate::ui::chat::render_chat;
use crate::ui::command_popup::render_command_popup;
use crate::ui::header::render_header;
use crate::ui::help::render_help_modal;
use crate::ui::metrics::render_metrics_bar;
use crate::ui::picker_modal::render_picker_modal;
use crate::ui::question_modal::render_question_modal;
use crate::ui::status_bar::{render_input, render_status_bar};
use crate::ui::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App, theme: &Theme) {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Header status line
            Constraint::Min(4),    // Full-width dialogue stream
            // Token / cache / speed readout, above the prompt where the eye
            // already is. Costs a transcript row, so it can be turned off.
            Constraint::Length(u16::from(app.metrics.enabled)),
            Constraint::Length(1), // Inline prompt line (❯ ...)
            Constraint::Length(1), // Status footer
        ])
        .split(f.area());

    // 1. Render Header (Top status line)
    render_header(f, app, main_chunks[0], theme);

    // 2. Render Main Dialogue Stream (Full screen width, no sidebar)
    {
        render_chat(f, app, main_chunks[1], theme);
    }

    // 3. Render the metrics bar
    render_metrics_bar(f, app, main_chunks[2], theme);

    // 4. Render the interactive inline prompt line
    render_input(f, app, main_chunks[3], theme);

    // 5. Render Slash Command Autocomplete Popover (floating above the prompt line)
    render_command_popup(f, app, main_chunks[3], theme);

    // 6. Render Minimal Status Footer
    render_status_bar(f, app, main_chunks[4], theme);

    // 6. Render Interactive Picker Dropdown Modal if active
    if app.picker.is_open {
        render_picker_modal(f, app, theme);
    }

    // 7. Render the ask_user_question modal (topmost: the agent is blocked on it)
    if app.pending_question.is_some() {
        render_question_modal(f, app, theme);
    }

    // 8. Render Help Modal Overlay if active
    if app.mode == ViewMode::Help {
        render_help_modal(f, f.area(), theme);
    }
}
