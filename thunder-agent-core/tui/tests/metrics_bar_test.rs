//! The metrics bar: that it renders, what it says, and that it can be turned
//! off without leaving a hole in the layout.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::tempdir;
use thunder_agent_loop::types::event::{AgentStats, TurnStats};
use thunder_tui::prelude::*;

/// Draws a frame and returns the screen rows as text.
///
/// A double-width character occupies two cells: the second holds a blank
/// placeholder, so rows are reconstructed by skipping it. Without this a
/// Chinese label reads back as `上 下 文`.
fn draw_rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &theme))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let width = buffer.area().width as usize;
    buffer
        .content()
        .chunks(width)
        .map(|row| {
            let mut out = String::new();
            let mut skip = 0usize;
            for cell in row {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = cell.symbol();
                out.push_str(symbol);
                let cells = unicode_width::UnicodeWidthStr::width(symbol);
                if cells > 1 {
                    skip = cells - 1;
                }
            }
            out
        })
        .collect()
}

/// Index of the prompt line (`❯ …`), which sits below the bar.
fn input_row(rows: &[String]) -> usize {
    rows.iter()
        .position(|r| r.contains('❯'))
        .expect("prompt row")
}

/// The input box pads its text, so the row above the prompt is not the bar.
const INPUT_VPAD: usize = thunder_tui::ui::status_bar::INPUT_VPAD as usize;

/// Every row the bar paints, joined. The layout reserves exactly the height
/// [`thunder_tui::ui::metrics::bar_height`] reports, so the rows can be located
/// without guessing or scanning for a background colour (the footer shares it).
fn bar_text(app: &App, rows: &[String], width: u16) -> String {
    let end = input_row(rows) - INPUT_VPAD;
    let height =
        thunder_tui::ui::metrics_height(app, &Theme::default(), width, rows.len() as u16) as usize;
    rows[end - height..end].join("\n")
}

fn app_with_run(workspace: &std::path::Path) -> App {
    let mut app = App::new("gpt-4o");
    app.workspace_dir = workspace.to_path_buf();
    app.thinking_level = Some("high".to_string());
    app.conversation.messages.clear();
    app.metrics.record_turn(&TurnStats {
        prompt_tokens: Some(12_000),
        completion_tokens: Some(1_500),
        cached_tokens: Some(11_000),
        ..Default::default()
    });
    app.metrics.begin_run();
    app.metrics.finish_run(Some(&AgentStats {
        total_turns: 2,
        total_prompt_tokens: 12_000,
        total_completion_tokens: 1_500,
        total_cached_tokens: 11_000,
        total_reasoning_tokens: 300,
        avg_tokens_per_second: Some(41.7),
        ..Default::default()
    }));
    app
}

#[tokio::test]
async fn the_bar_sits_on_the_row_above_the_prompt() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());

    let rows = draw_rows(&mut app, 200, 24);
    let bar = bar_text(&app, &rows, 200);

    assert!(bar.contains("tok/s"), "speed: {bar:?}");
    assert!(bar.contains("Cache"), "cache: {bar:?}");
    // The context segment carries the number without a label of its own.
    assert!(bar.contains("13,500"), "context: {bar:?}");
    assert!(bar.contains("Total"), "session total: {bar:?}");
    assert!(bar.contains("Turn"), "turn breakdown: {bar:?}");
    assert!(bar.contains("Elapsed"), "duration: {bar:?}");
    assert!(bar.contains("openai/gpt-4o"), "model: {bar:?}");
    assert!(bar.contains("think:high"), "thinking level: {bar:?}");
    assert!(!bar.contains("上下文"), "labels are English: {bar:?}");
    // Order: the workspace stays first, the model and its thinking level come
    // next, ahead of the telemetry they explain.
    let order: Vec<usize> = ["📁", "🤖", "🧠", "Total"]
        .iter()
        .map(|marker| {
            bar.find(marker)
                .unwrap_or_else(|| panic!("{marker} missing: {bar:?}"))
        })
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "order: {bar:?}");
    // The workspace is abbreviated but its distinguishing tail survives.
    let tail = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(bar.contains(&tail), "workspace: {bar:?}");
}

#[tokio::test]
async fn the_bar_reports_the_numbers_the_engine_reported() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());

    let rows = draw_rows(&mut app, 200, 24);
    let bar = bar_text(&app, &rows, 200);

    // 12,000 prompt + 1,500 completion, cached 11,000 of 12,000 = 91.67%.
    assert!(bar.contains("41.7"), "avg tps: {bar:?}");
    assert!(bar.contains("13,500"), "turn total: {bar:?}");
    assert!(bar.contains("12,000"), "input tokens: {bar:?}");
    assert!(bar.contains("1,500"), "output tokens: {bar:?}");
    assert!(bar.contains("91.67%"), "cache hit rate: {bar:?}");
    assert!(bar.contains("think 300"), "reasoning: {bar:?}");
}

#[tokio::test]
async fn turning_the_bar_off_returns_the_row_to_the_transcript() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());

    let on = draw_rows(&mut app, 120, 24);
    let on_bar = bar_text(&app, &on, 120);
    assert!(on_bar.contains("Total"), "bar is drawn: {on_bar:?}");

    app.metrics.enabled = false;
    let off = draw_rows(&mut app, 120, 24);
    let off_above = off[input_row(&off) - 1 - INPUT_VPAD].clone();
    assert!(
        !off_above.contains("Total"),
        "the bar is gone: {off_above:?}"
    );
    // What takes its place is the transcript's bottom border.
    assert!(off_above.contains('─'), "border moves up: {off_above:?}");
}

#[tokio::test]
async fn a_narrow_bar_wraps_instead_of_dropping_telemetry() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());

    for width in [24u16, 30, 40, 60] {
        let rows = draw_rows(&mut app, width, 24);
        let bar = bar_text(&app, &rows, width);

        // Everything the wide bar says survives the wrap, not just the items
        // that happened to fit on the first row.
        for marker in [
            "📁", "🤖", "🧠", "13,500", "Cache", "Total", "Turn", "Elapsed",
        ] {
            assert!(
                bar.contains(marker),
                "width {width}: {marker} went missing: {bar:?}"
            );
        }
        // ...and no line spills past the pane.
        for line in bar.lines() {
            assert!(
                unicode_width::UnicodeWidthStr::width(line) <= width as usize,
                "width {width}: {line:?} overflows"
            );
        }
    }
}

#[tokio::test]
async fn the_bar_yields_rows_to_the_transcript_on_a_short_frame() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());

    // 40 columns want five rows; a ten-row frame can only spare three once the
    // header, the transcript's floor, the editor and the footer are paid for.
    let rows = draw_rows(&mut app, 40, 10);
    let bar_rows = bar_text(&app, &rows, 40).lines().count();
    assert_eq!(bar_rows, 3, "capped by the frame: {:?}", rows);
    for line in bar_text(&app, &rows, 40).lines() {
        assert!(unicode_width::UnicodeWidthStr::width(line) <= 40);
    }
}

#[tokio::test]
async fn a_slash_command_toggles_it() {
    let dir = tempdir().unwrap();
    let mut app = app_with_run(dir.path());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    assert!(app.metrics.enabled, "on by default");
    app.execute_slash_command("/metrics off", tx.clone());
    assert!(!app.metrics.enabled);
    app.execute_slash_command("/metrics", tx);
    assert!(app.metrics.enabled, "bare /metrics toggles back");
}
