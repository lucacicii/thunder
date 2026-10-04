//! The token / cache / speed readout, mirroring the panel's metrics bar.
//!
//! The panel keeps a permanently visible strip above its composer showing the
//! model's throughput, cache hit, context occupancy and running totals. This is
//! the same information in one terminal row, and the formatters are
//! deliberately the same arithmetic so both surfaces print the same strings for
//! the same run.
//!
//! Width is the whole design problem here: a terminal row is ~40–200 cells and
//! there are seven things to say. Segments carry a priority and are dropped
//! from the least important end until the rest fit, so a narrow window loses
//! telemetry instead of losing the line.

use crate::app::App;
use crate::ui::theme::Theme;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use std::path::Path;
use std::time::Instant;
use thunder_agent_loop::types::event::{AgentStats, TurnStats};
use unicode_width::UnicodeWidthStr;

/// Divider between two segments.
const SEPARATOR: &str = " · ";
const SEPARATOR_WIDTH: usize = 3;
/// Longest workspace string before it is abbreviated from the left.
const WORKSPACE_MAX: usize = 40;
/// Context occupancy at or above this is drawn as a warning.
const CONTEXT_WARN_PERCENT: f64 = 80.0;

// ── Formatters (same arithmetic as the panel's) ─────────────────────────────

/// `12` / `45k` / `1.2M`. The panel's `formatCompactTokens`.
pub fn format_compact_tokens(num: usize) -> String {
    if num >= 1_000_000 {
        let val = num as f64 / 1_000_000.0;
        if val.fract() == 0.0 {
            format!("{val:.0}M")
        } else {
            format!("{val:.1}M")
        }
    } else if num >= 1_000 {
        // The panel's `toFixed(0)` on this branch, so 12_400 reads as `12k`.
        format!("{:.0}k", num as f64 / 1_000.0)
    } else {
        num.to_string()
    }
}

/// Thousands separators: `12345` → `12,345`.
pub fn format_thousands(num: usize) -> String {
    let digits = num.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Below 100k the panel uses grouped digits; above it, the compact form.
pub fn format_token_count(num: usize) -> String {
    if num >= 100_000 {
        format_compact_tokens(num)
    } else {
        format_thousands(num)
    }
}

/// `3.2s` / `45s` / `1m 05s` / `1h 02m`. The panel's `formatDuration`.
pub fn format_duration(ms: u64) -> String {
    let total_sec = ms as f64 / 1000.0;
    if total_sec < 60.0 {
        let sec = total_sec.floor();
        // Sub-minute durations keep one decimal, but only once it is meaningful.
        if total_sec - sec >= 0.05 {
            format!("{total_sec:.1}s")
        } else {
            format!("{}s", sec as u64)
        }
    } else {
        let total_min = (total_sec / 60.0).floor();
        if total_min < 60.0 {
            let rem_sec = (total_sec % 60.0).round();
            if rem_sec >= 60.0 {
                format!("{}m", total_min as u64 + 1)
            } else {
                format!("{}m {:02}s", total_min as u64, rem_sec as u64)
            }
        } else {
            let hours = (total_min / 60.0).floor();
            let rem_min = (total_min % 60.0).round();
            if rem_min >= 60.0 {
                format!("{}h", hours as u64 + 1)
            } else {
                format!("{}h {:02}m", hours as u64, rem_min as u64)
            }
        }
    }
}

/// `42.3`, matching the panel's `toFixed(1)`.
pub fn format_tps(tps: f64) -> String {
    format!("{tps:.1}")
}

/// Abbreviates a path from the left: `$HOME` becomes `~`, and a long path keeps
/// its tail behind an ellipsis, because the tail is what distinguishes it.
pub fn shorten_path(path: &Path, max: usize) -> String {
    let text = path.display().to_string();
    let mut shown = match std::env::var("HOME") {
        Ok(home) if text == home => "~".to_string(),
        Ok(home) if text.starts_with(&format!("{home}/")) => {
            format!("~/{}", &text[home.len() + 1..])
        }
        _ => text,
    };
    if UnicodeWidthStr::width(shown.as_str()) <= max {
        return shown;
    }
    // Keep whole trailing components.
    let parts: Vec<&str> = shown.split('/').collect();
    let mut tail = String::new();
    for part in parts.iter().rev() {
        let candidate = if tail.is_empty() {
            (*part).to_string()
        } else {
            format!("{part}/{tail}")
        };
        if UnicodeWidthStr::width(candidate.as_str()) + 2 > max {
            break;
        }
        tail = candidate;
    }
    shown = format!("…/{tail}");
    shown
}

// ── State ───────────────────────────────────────────────────────────────────

/// Live and last-run telemetry for the bar.
///
/// Deliberately a value type with no rendering in it, so the numbers can be
/// asserted in tests without a terminal.
#[derive(Debug, Default)]
pub struct Metrics {
    /// Show the bar. Off restores the row to the transcript.
    pub enabled: bool,
    /// Delta events seen this run. The panel counts events the same way, so the
    /// two surfaces report the same figure for the same stream.
    pub live_tokens: usize,
    pub live_reasoning: usize,
    /// When the first delta of this run arrived; the tps clock starts here, not
    /// at submit, so queueing and prompt processing are not billed as latency
    /// the model did not have.
    first_token_at: Option<Instant>,
    /// Wall clock from submit to the run finishing, matching the panel's
    /// "总耗时" (which includes tool execution and queueing).
    run_started_at: Option<Instant>,
    pub last_run_wall_ms: Option<u64>,
    pub last_run: Option<AgentStats>,
    pub last_turn: Option<TurnStats>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }

    /// A new run starts: keep the last run's numbers on display until they are
    /// replaced, but reset everything that belongs to the live stream.
    pub fn begin_run(&mut self) {
        self.live_tokens = 0;
        self.live_reasoning = 0;
        self.first_token_at = None;
        self.run_started_at = Some(Instant::now());
        self.last_run_wall_ms = None;
    }

    pub fn count_token_delta(&mut self) {
        self.first_token_at.get_or_insert_with(Instant::now);
        self.live_tokens += 1;
    }

    pub fn count_reasoning_delta(&mut self) {
        self.first_token_at.get_or_insert_with(Instant::now);
        self.live_reasoning += 1;
    }

    pub fn record_turn(&mut self, stats: &TurnStats) {
        self.last_turn = Some(stats.clone());
    }

    /// A different conversation: drop everything that described the old one,
    /// keeping the user's preference about whether the bar is shown at all.
    pub fn reset_for_session(&mut self) {
        self.live_tokens = 0;
        self.live_reasoning = 0;
        self.first_token_at = None;
        self.run_started_at = None;
        self.last_run_wall_ms = None;
        self.last_run = None;
        self.last_turn = None;
    }

    pub fn finish_run(&mut self, stats: Option<&AgentStats>) {
        self.last_run_wall_ms = self
            .run_started_at
            .map(|start| start.elapsed().as_millis() as u64);
        self.run_started_at = None;
        if let Some(stats) = stats {
            self.last_run = Some(stats.clone());
        }
        self.first_token_at = None;
    }

    /// Tokens/second for the live stream. Zero until the stream has run long
    /// enough to mean anything (the panel uses the same 0.2s floor).
    pub fn live_tps(&self) -> f64 {
        let Some(started) = self.first_token_at else {
            return 0.0;
        };
        if self.live_tokens == 0 {
            return 0.0;
        }
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed <= 0.2 {
            return 0.0;
        }
        self.live_tokens as f64 / elapsed
    }

    /// Context occupancy: the last turn's provider-reported volume, falling back
    /// to the working-context estimate before any turn has completed. This is
    /// the panel's figure, which is why a compacted session can differ from the
    /// message-list estimate.
    pub fn context_tokens(&self, estimated: usize) -> usize {
        match &self.last_turn {
            Some(turn) => match (turn.prompt_tokens, turn.completion_tokens) {
                (None, None) => estimated,
                (p, c) => p.unwrap_or(0) + c.unwrap_or(0),
            },
            None => estimated,
        }
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// One item of the bar.
///
/// Segments are built in decreasing order of importance and [`fit`] keeps a
/// prefix of that order, so the field order *is* the drop order.
struct Segment {
    spans: Vec<Span<'static>>,
    width: usize,
}

impl Segment {
    fn new(spans: Vec<Span<'static>>) -> Self {
        let width = spans
            .iter()
            .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
            .sum();
        Self { spans, width }
    }

    #[cfg(test)]
    fn text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_ref()).collect()
    }
}

pub fn render_metrics_bar(f: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    if !app.metrics.enabled || area.height == 0 {
        return;
    }
    let segments = build_segments(app, theme, area.width as usize);
    let kept = fit(segments, area.width as usize);

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, segment) in kept.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(SEPARATOR, theme.muted_style()));
        }
        spans.extend(segment.spans);
    }

    let paragraph =
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(10, 14, 20)));
    f.render_widget(paragraph, area);
}

/// Everything the bar can say, in display order.
///
/// `width` is needed on the way in because the workspace is abbreviated to fit
/// the bar rather than to a fixed budget: on a narrow window an over-long
/// workspace would be the item dropped first, which is the opposite of useful.
fn build_segments(app: &App, theme: &Theme, width: usize) -> Vec<Segment> {
    let label = Style::default().fg(theme.text_muted);
    let value = Style::default()
        .fg(theme.text_main)
        .add_modifier(Modifier::BOLD);
    let metric = &app.metrics;
    let mut segments = Vec::new();

    // 1. Workspace: where write operations actually land, and the thing that
    //    silently differs between two terminals showing the same model.
    if let Some(dir) = app.workspace_dir.to_str() {
        let budget = (width / 3).clamp(12, WORKSPACE_MAX);
        let mut text = shorten_path(Path::new(dir), budget);
        if !app.extra_roots.is_empty() {
            text.push_str(&format!(" +{}", app.extra_roots.len()));
        }
        segments.push(Segment::new(vec![
            Span::styled("📁 ", label),
            Span::styled(text, Style::default().fg(theme.accent_primary)),
        ]));
    }

    // 2. Context occupancy against the model's window.
    let context = metric.context_tokens(app.conversation.stats.total_tokens);
    let window = app
        .provider_registry
        .resolve(&app.model.selection_id())
        .map(|spec| spec.context_window)
        .filter(|w| *w > 0);
    let percent = window.map(|w| context as f64 / w as f64 * 100.0);
    let mut context_spans = vec![
        Span::styled("上下文 ", label),
        Span::styled(
            match window {
                Some(w) => format!(
                    "{} / {}",
                    format_token_count(context),
                    format_compact_tokens(w)
                ),
                None => format_token_count(context),
            },
            value,
        ),
    ];
    if let Some(p) = percent {
        context_spans.push(Span::styled(
            format!(" ({p:.0}%)"),
            if p >= CONTEXT_WARN_PERCENT {
                Style::default()
                    .fg(theme.error_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                label
            },
        ));
    }
    segments.push(Segment::new(context_spans));

    // 3. Speed: live while streaming, the last run's average once it settles.
    let running = app.is_running();
    let speed_spans = if running {
        let mut spans = vec![Span::styled(
            format!("⚡ {} tok/s", format_tps(metric.live_tps())),
            Style::default()
                .fg(theme.tool_bubble)
                .add_modifier(Modifier::BOLD),
        )];
        let mut live = format!("{} tok", format_token_count(metric.live_tokens));
        if metric.live_reasoning > 0 {
            live.push_str(&format!(
                " (思考 {})",
                format_token_count(metric.live_reasoning)
            ));
        }
        spans.push(Span::styled(format!("  {live}"), label));
        spans
    } else if let Some(tps) = metric
        .last_run
        .as_ref()
        .and_then(|s| s.avg_tokens_per_second)
    {
        if tps > 0.0 {
            vec![Span::styled(
                format!("⚡ {} tok/s", format_tps(tps)),
                Style::default()
                    .fg(theme.tool_bubble)
                    .add_modifier(Modifier::BOLD),
            )]
        } else {
            vec![Span::styled("⚡ Ready", label)]
        }
    } else {
        vec![Span::styled("⚡ Ready", label)]
    };
    segments.push(Segment::new(speed_spans));

    // 4. Prompt cache. Shown from the last run's numbers, which is what the
    //    panel does while streaming too — this turn's cache figures only exist
    //    once the provider has answered.
    let cache = metric.last_run.as_ref().map(|s| {
        let cached = s.total_cached_tokens;
        let prompt = s.total_prompt_tokens;
        let pct = (prompt > 0 && cached > 0).then(|| cached as f64 / prompt as f64 * 100.0);
        let hit = cached > 0;
        (cached, pct, hit)
    });
    if let Some((cached, pct, hit)) = cache {
        let mut spans = vec![Span::styled("Cache ", label)];
        spans.push(Span::styled(
            format_token_count(cached),
            if hit {
                value
            } else {
                Style::default().fg(theme.text_muted)
            },
        ));
        if let Some(p) = pct {
            spans.push(Span::styled(format!(" ({p:.2}%)"), label));
        }
        segments.push(Segment::new(spans));
    }

    // 5. Cumulative billed usage for the whole conversation.
    segments.push(Segment::new(vec![
        Span::styled("总消耗 ", label),
        Span::styled(
            format_token_count(app.conversation.stats.total_used_tokens),
            value,
        ),
    ]));

    // 6. This run's token breakdown.
    if let Some(stats) = metric.last_run.as_ref() {
        let prompt = stats.total_prompt_tokens;
        let completion = stats.total_completion_tokens;
        let total = prompt + completion;
        if total > 0 || prompt > 0 || completion > 0 {
            let mut spans = vec![
                Span::styled("Turn ", label),
                Span::styled(format_token_count(total), value),
                Span::styled(
                    format!(
                        " (In {} · Out {}",
                        format_token_count(prompt),
                        format_token_count(completion)
                    ),
                    label,
                ),
            ];
            if stats.total_reasoning_tokens > 0 {
                spans.push(Span::styled(
                    format!(
                        " (思考 {})",
                        format_token_count(stats.total_reasoning_tokens)
                    ),
                    label,
                ));
            }
            spans.push(Span::styled(")", label));
            segments.push(Segment::new(spans));
        }
    }

    // 7. Wall clock of the last run.
    if let Some(ms) = metric.last_run_wall_ms {
        segments.push(Segment::new(vec![
            Span::styled("耗时 ", label),
            Span::styled(format_duration(ms), value),
        ]));
    }

    segments
}

/// Keeps the longest leading run of segments that fits `width`.
///
/// Deliberately a strict prefix. A greedy scan that skipped an over-wide
/// segment could keep a *less* important one instead — showing `耗时` while
/// dropping `总消耗` — which reads as a bug rather than as degradation.
fn fit(segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    let mut used = 0usize;
    let mut kept = Vec::new();
    for segment in segments {
        let extra = if kept.is_empty() {
            segment.width
        } else {
            segment.width + SEPARATOR_WIDTH
        };
        if used + extra > width {
            break;
        }
        used += extra;
        kept.push(segment);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_tokens_matches_the_panel() {
        assert_eq!(format_compact_tokens(0), "0");
        assert_eq!(format_compact_tokens(999), "999");
        assert_eq!(format_compact_tokens(1_000), "1k");
        assert_eq!(format_compact_tokens(12_400), "12k");
        assert_eq!(format_compact_tokens(999_999), "1000k");
        assert_eq!(format_compact_tokens(1_000_000), "1M");
        assert_eq!(format_compact_tokens(1_250_000), "1.2M");
    }

    #[test]
    fn token_count_switches_to_compact_at_100k() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(12_345), "12,345");
        assert_eq!(format_token_count(99_999), "99,999");
        assert_eq!(format_token_count(100_000), "100k");
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(format_thousands(1), "1");
        assert_eq!(format_thousands(999), "999");
        assert_eq!(format_thousands(1_000), "1,000");
        assert_eq!(format_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn durations_match_the_panel() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(1_000), "1s");
        assert_eq!(format_duration(3_200), "3.2s");
        // Under 50ms of fraction the decimal is dropped rather than shown as .0
        assert_eq!(format_duration(1_030), "1s");
        assert_eq!(format_duration(45_000), "45s");
        assert_eq!(format_duration(59_960), "60.0s");
        assert_eq!(format_duration(60_000), "1m 00s");
        assert_eq!(format_duration(65_000), "1m 05s");
        assert_eq!(format_duration(3_600_000), "1h 00m");
        assert_eq!(format_duration(3_723_000), "1h 02m");
    }

    #[test]
    fn tps_keeps_one_decimal() {
        assert_eq!(format_tps(0.0), "0.0");
        assert_eq!(format_tps(42.34), "42.3");
        assert_eq!(format_tps(1234.5), "1234.5");
    }

    #[test]
    fn workspace_is_shortened_from_the_left() {
        // Whole trailing components survive; the head is what goes.
        let short = shorten_path(Path::new("/a/b/c/d/e/f/g/h/i/j/k"), 12);
        assert!(short.starts_with("…/"), "got {short:?}");
        assert!(
            UnicodeWidthStr::width(short.as_str()) <= 12,
            "got {short:?}"
        );
        assert!(
            short.ends_with("k"),
            "keeps the distinguishing tail: {short:?}"
        );
        // A path that fits is untouched.
        assert_eq!(shorten_path(Path::new("/tmp/x"), 40), "/tmp/x");
    }

    #[test]
    fn workspace_collapses_home() {
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        assert_eq!(shorten_path(Path::new(&home), 40), "~");
        assert_eq!(
            shorten_path(Path::new(&format!("{home}/wz/ag")), 40),
            "~/wz/ag"
        );
    }

    #[test]
    fn live_tps_needs_a_measurable_window() {
        let mut m = Metrics::new();
        assert_eq!(m.live_tps(), 0.0, "nothing streamed yet");
        m.count_token_delta();
        // The clock just started, so the panel (and this) reports 0.
        assert_eq!(m.live_tps(), 0.0, "too early to mean anything");
    }

    #[test]
    fn live_counters_track_the_two_streams() {
        let mut m = Metrics::new();
        m.begin_run();
        for _ in 0..3 {
            m.count_token_delta();
        }
        m.count_reasoning_delta();
        assert_eq!(m.live_tokens, 3);
        assert_eq!(m.live_reasoning, 1);
        // A new run starts the counters over.
        m.begin_run();
        assert_eq!(m.live_tokens, 0);
        assert_eq!(m.live_reasoning, 0);
    }

    #[test]
    fn context_prefers_the_last_turn_over_the_estimate() {
        let mut m = Metrics::new();
        assert_eq!(m.context_tokens(500), 500, "no turn yet: use the estimate");
        m.record_turn(&TurnStats {
            prompt_tokens: Some(900),
            completion_tokens: Some(100),
            ..Default::default()
        });
        assert_eq!(m.context_tokens(500), 1_000, "provider-reported wins");
        // A turn with no reported usage falls back rather than reading as zero.
        m.record_turn(&TurnStats::default());
        assert_eq!(m.context_tokens(500), 500);
    }

    #[test]
    fn finishing_a_run_records_the_wall_clock() {
        let mut m = Metrics::new();
        m.begin_run();
        m.finish_run(Some(&AgentStats {
            total_turns: 2,
            total_prompt_tokens: 1_000,
            total_completion_tokens: 200,
            ..Default::default()
        }));
        assert!(m.last_run_wall_ms.is_some(), "wall clock is recorded");
        assert_eq!(m.last_run.as_ref().unwrap().total_turns, 2);
    }

    #[test]
    fn segments_drop_the_least_important_when_narrow() {
        let mut app = App::new("gpt-4o");
        app.conversation.messages.clear();
        app.conversation.add_user_message("hello");
        app.metrics.record_turn(&TurnStats {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(200),
            ..Default::default()
        });
        app.metrics.begin_run();
        app.metrics.finish_run(Some(&AgentStats {
            total_turns: 1,
            total_prompt_tokens: 1_000,
            total_completion_tokens: 200,
            total_cached_tokens: 900,
            avg_tokens_per_second: Some(42.0),
            ..Default::default()
        }));
        let theme = Theme::default();

        let all = build_segments(&app, &theme, 200);
        assert_eq!(all.len(), 7, "every item has something to report");

        // Wide: everything fits.
        assert_eq!(fit(build_segments(&app, &theme, 300), 300).len(), all.len());

        // Narrow: a leading run survives, and it really is the leading run —
        // compare item by item, not just sizes.
        let narrow = fit(build_segments(&app, &theme, 24), 24);
        assert!(!narrow.is_empty(), "something must remain");
        assert!(
            narrow.len() < all.len(),
            "a 24-cell bar cannot hold everything"
        );
        for (kept, built) in narrow.iter().zip(build_segments(&app, &theme, 24).iter()) {
            assert_eq!(
                kept.text(),
                built.text(),
                "kept items are the leading ones, in order"
            );
        }
        assert!(
            narrow[0].text().contains('📁'),
            "the workspace outranks everything"
        );
    }

    #[test]
    fn chosen_segments_always_fit_the_width() {
        let mut app = App::new("gpt-4o");
        app.conversation.add_user_message("x".repeat(400));
        app.metrics.enabled = true;
        let theme = Theme::default();

        for width in [10usize, 20, 40, 60, 80, 120, 200] {
            let kept = fit(build_segments(&app, &theme, width), width);
            let total: usize = kept.iter().map(|s| s.width).sum::<usize>()
                + SEPARATOR_WIDTH * kept.len().saturating_sub(1);
            assert!(total <= width, "width {width}: {total} cells requested");
        }
    }

    #[test]
    fn the_estimate_and_the_message_list_stay_in_touch() {
        // Guard against the bar reading a field that is never populated.
        let mut app = App::new("gpt-4o");
        app.conversation.messages.clear();
        app.conversation.add_user_message("a message");
        assert!(app.conversation.stats.total_tokens > 0);
    }
}
