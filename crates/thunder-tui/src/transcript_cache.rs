//! Per-item transcript render cache.
//!
//! A frame used to re-parse the entire conversation as Markdown, every time.
//! Measured on 120 items (40 turns with code blocks and tables): **5.6 ms per
//! frame in release, 42.8 ms in debug** — and streaming one extra message cost
//! essentially nothing, proving the time went to history that had not changed.
//!
//! The cache renders each timeline item once, then reuses the lines until that
//! item's content actually differs or the terminal width changes (wrapping is
//! width-dependent). Fingerprints are cheap hashes; the markdown parse they
//! replace is orders of magnitude more expensive.

use crate::app::TimelineItem;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use unicode_width::UnicodeWidthChar;

#[derive(Default)]
pub struct TranscriptCache {
    width: u16,
    items: Vec<CachedItem>,
}

struct CachedItem {
    fingerprint: u64,
    lines: Vec<Line<'static>>,
}

impl TranscriptCache {
    /// Drop everything: the next frame renders from scratch.
    pub fn invalidate(&mut self) {
        self.items.clear();
    }

    /// How many items are cached (diagnostics and tests).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Lines for this transcript at this width, rendering only the items whose
    /// content changed since the previous frame.
    ///
    /// The result is **pre-wrapped**: every returned `Line` fits the pane, so the
    /// widget does not have to re-wrap the whole transcript on every frame.
    /// (Measured: `Paragraph::wrap` was 8 of 10 ms per frame at 120 items in a
    /// debug build; pre-wrapping once per changed item removes it.)
    pub fn lines(
        &mut self,
        timeline: &[TimelineItem],
        width: u16,
        render: impl Fn(&TimelineItem, u16) -> Vec<Line<'static>>,
    ) -> Vec<Line<'static>> {
        // Wrapping is width-dependent, so a resize invalidates every line.
        if self.width != width {
            self.width = width;
            self.items.clear();
        }
        let wrap_width = width.saturating_sub(2).max(8) as usize; // pane borders

        self.items.truncate(timeline.len());

        let mut out = Vec::new();
        for (index, item) in timeline.iter().enumerate() {
            let fingerprint = fingerprint(item);
            let needs_render = match self.items.get(index) {
                Some(cached) => cached.fingerprint != fingerprint,
                None => true,
            };

            if needs_render {
                let rendered = render(item, width);
                let lines = wrap_lines(rendered, wrap_width);
                let entry = CachedItem { fingerprint, lines };
                if index < self.items.len() {
                    self.items[index] = entry;
                } else {
                    self.items.push(entry);
                }
            }

            out.extend(self.items[index].lines.iter().cloned());
        }
        out
    }
}

/// Content hash for one timeline item.
///
/// Only the fields that reach the screen participate, so a status flip on a
/// tool card or a growing streaming message invalidates exactly one item.
fn fingerprint(item: &TimelineItem) -> u64 {
    let mut hasher = DefaultHasher::new();
    match item {
        TimelineItem::User { text } => {
            0u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        TimelineItem::Assistant {
            content,
            reasoning,
            streaming,
        } => {
            1u8.hash(&mut hasher);
            content.hash(&mut hasher);
            reasoning.hash(&mut hasher);
            streaming.hash(&mut hasher);
        }
        TimelineItem::ToolCallCard {
            call_id,
            name,
            args_summary,
            status,
        } => {
            2u8.hash(&mut hasher);
            call_id.hash(&mut hasher);
            name.hash(&mut hasher);
            args_summary.hash(&mut hasher);
            match status {
                crate::app::ToolCardStatus::Running => 0u8.hash(&mut hasher),
                crate::app::ToolCardStatus::Success {
                    output_preview,
                    duration_ms,
                } => {
                    1u8.hash(&mut hasher);
                    output_preview.hash(&mut hasher);
                    duration_ms.hash(&mut hasher);
                }
                crate::app::ToolCardStatus::Error {
                    error_preview,
                    duration_ms,
                } => {
                    2u8.hash(&mut hasher);
                    error_preview.hash(&mut hasher);
                    duration_ms.hash(&mut hasher);
                }
            }
        }
        TimelineItem::SystemNotice { text } => {
            3u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Split rendered lines so none exceeds `width` terminal cells.
///
/// Styling is preserved: a span that straddles a break becomes two spans with
/// the same style. Breaks prefer the last space on the line, and a single word
/// longer than the pane is hard-broken rather than truncated.
fn wrap_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if line.width() <= width {
            out.push(line);
            continue;
        }

        // Flatten to (char, style) so a break can fall inside a span.
        let mut cells: Vec<(char, Style)> = Vec::new();
        for span in &line.spans {
            for ch in span.content.chars() {
                cells.push((ch, span.style));
            }
        }

        let mut start = 0usize;
        while start < cells.len() {
            let mut end = start;
            let mut used = 0usize;
            let mut last_space: Option<usize> = None;
            while end < cells.len() {
                let (ch, _) = cells[end];
                let w = UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + w > width {
                    break;
                }
                used += w;
                if ch == ' ' {
                    last_space = Some(end);
                }
                end += 1;
            }

            // Prefer breaking at the last space instead of mid-word.
            if end < cells.len() {
                if let Some(space) = last_space {
                    if space > start {
                        out.push(line_from_cells(&cells[start..space]));
                        start = space + 1;
                        continue;
                    }
                }
            }
            if end == start {
                end = start + 1; // a single glyph wider than the pane
            }
            out.push(line_from_cells(&cells[start..end]));
            start = end;
        }
    }
    out
}

fn line_from_cells(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_style: Option<Style> = None;
    let mut current = String::new();
    for (ch, style) in cells {
        if current_style != Some(*style) {
            if !current.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current),
                    current_style.unwrap_or_default(),
                ));
            }
            current_style = Some(*style);
        }
        current.push(*ch);
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, current_style.unwrap_or_default()));
    }
    Line::from(spans)
}
