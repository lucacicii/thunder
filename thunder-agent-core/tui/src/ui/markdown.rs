//! A small Markdown renderer for the chat stream.
//!
//! Not a CommonMark implementation — a *preview* renderer for what models
//! actually emit. Two properties matter more than completeness:
//!
//! * **Lenient.** The streaming delta ends mid-construct (`**bo`, an unclosed
//!   fence). Every block and inline rule degrades to plain text rather than
//!   failing or re-flowing the whole message once per token.
//! * **Width-exact.** Lines are wrapped here, not by `Paragraph`, so each
//!   rendered `Line` is exactly one screen row. That makes the scroll bound and
//!   a link's screen position exact, which is what click-to-reveal needs.
//!
//! Styling comes entirely from [`Theme`]; the module knows no colours itself.

use crate::links::{classify, LinkCache, LinkTarget};
use crate::ui::theme::Theme;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::PathBuf;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Left margin every rendered line carries, so the chat stream and the prompt
/// line share an edge.
const INDENT: &str = "  ";
/// A single fenced code line is capped at this many rows before `…`.
const CODE_MAX_ROWS: usize = 3;
/// Table columns are capped so one long cell cannot push the rest off-screen.
const TABLE_CELL_MAX: usize = 40;

/// Where a clickable run sits on a rendered line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    /// Index into [`Rendered::lines`].
    pub line: usize,
    /// Inclusive start column, in terminal cells, from the line's left edge.
    pub col_start: u16,
    /// Exclusive end column.
    pub col_end: u16,
    pub target: LinkTarget,
}

/// The output of one render pass.
#[derive(Default)]
pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub links: Vec<LinkSpan>,
}

/// Everything the renderer needs beyond the text itself.
pub struct RenderCtx<'a> {
    pub theme: &'a Theme,
    /// Usable width in terminal cells (already excludes the pane borders).
    pub width: usize,
    /// Directories a relative path may resolve in: workspace first.
    pub roots: &'a [PathBuf],
    pub cache: &'a mut LinkCache,
}

/// Renders Markdown source into styled, pre-wrapped lines plus link hitboxes.
pub fn render(text: &str, ctx: &mut RenderCtx<'_>) -> Rendered {
    let mut out = Rendered::default();
    for block in parse_blocks(text) {
        render_block(block, ctx, &mut out);
    }
    out
}

// ── Fragments and cells ─────────────────────────────────────────────────────

/// A styled run of text before wrapping.
#[derive(Clone)]
struct Frag {
    text: String,
    style: Style,
    /// Raw target text from markup (`[t](href)`, `` `path` ``) still to resolve.
    target: Option<String>,
    link: Option<LinkTarget>,
    /// Came from an inline-code span, so it takes the code style.
    code: bool,
}

impl Frag {
    fn plain(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
            target: None,
            link: None,
            code: false,
        }
    }

    fn with_target(text: impl Into<String>, style: Style, target: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style,
            target: Some(target.into()),
            link: None,
            code: false,
        }
    }

    /// An inline-code span; `target` is the text to try to resolve as a path.
    fn code(text: impl Into<String>, style: Style, target: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style,
            target: Some(target.into()),
            link: None,
            code: true,
        }
    }
}

/// One terminal cell after wrapping.
#[derive(Clone)]
struct Cell {
    ch: char,
    style: Style,
    link: Option<LinkTarget>,
}

fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn text_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

// ── Block parsing ───────────────────────────────────────────────────────────

enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(String),
    Code {
        lang: String,
        lines: Vec<String>,
    },
    Quote(Vec<String>),
    ListItem {
        depth: usize,
        marker: String,
        text: String,
    },
    Rule,
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Blank,
}

/// Splits source into blocks. Deliberately line-oriented: a soft line break is
/// kept rather than joined, so the author's own layout survives.
fn parse_blocks(text: &str) -> Vec<Block> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks: Vec<Block> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim_end();

        if let Some((fence_char, fence_len, lang)) = fence_open(line) {
            let mut body = Vec::new();
            i += 1;
            // An unclosed fence (the streaming case) runs to the end: showing
            // the code is better than re-parsing it as prose each token.
            while i < lines.len() {
                if fence_close(lines[i].trim_end(), fence_char, fence_len) {
                    i += 1;
                    break;
                }
                body.push(lines[i].to_string());
                i += 1;
            }
            blocks.push(Block::Code { lang, lines: body });
            continue;
        }

        if let Some((level, text)) = heading(line) {
            blocks.push(Block::Heading { level, text });
            i += 1;
            continue;
        }

        if is_rule(line) {
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }

        if line.trim_start().starts_with('>') {
            let mut quoted = Vec::new();
            while i < lines.len() {
                let candidate = lines[i].trim_end();
                let Some(rest) = candidate.trim_start().strip_prefix('>') else {
                    break;
                };
                quoted.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                i += 1;
            }
            blocks.push(Block::Quote(quoted));
            continue;
        }

        if line.contains('|') && table_separator(lines.get(i + 1).copied().unwrap_or("")) {
            let header = split_row(line);
            i += 2; // header + separator
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                rows.push(split_row(lines[i]));
                i += 1;
            }
            blocks.push(Block::Table { header, rows });
            continue;
        }

        if let Some((depth, marker, text)) = list_item(line) {
            blocks.push(Block::ListItem {
                depth,
                marker,
                text,
            });
            i += 1;
            continue;
        }

        if line.trim().is_empty() {
            // Collapse runs of blank lines into one, the way Markdown does.
            if !matches!(blocks.last(), Some(Block::Blank)) {
                blocks.push(Block::Blank);
            }
            i += 1;
            continue;
        }

        blocks.push(Block::Paragraph(line.to_string()));
        i += 1;
    }

    blocks
}

/// `(fence_char, fence_len, lang)` for an opening ``` / ~~~ line.
fn fence_open(line: &str) -> Option<(char, usize, String)> {
    let trimmed = line.trim_start();
    let fence_char = trimmed.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = trimmed.chars().take_while(|c| *c == fence_char).count();
    if len < 3 {
        return None;
    }
    let lang = trimmed[len..].trim().to_string();
    Some((fence_char, len, lang))
}

fn fence_close(line: &str, fence_char: char, open_len: usize) -> bool {
    let trimmed = line.trim_start();
    let len = trimmed.chars().take_while(|c| *c == fence_char).count();
    len >= open_len && trimmed[len..].trim().is_empty()
}

/// `# Title` → `(1, "Title")`.
fn heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    Some((level as u8, rest.trim().to_string()))
}

/// `---`, `***`, `___` (three or more, nothing else).
fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    ['-', '*', '_'].into_iter().any(|c| {
        trimmed.len() >= 3
            && trimmed.chars().all(|x| x == c || x == ' ')
            && trimmed.chars().filter(|x| *x == c).count() >= 3
    })
}

/// `|---|---|` or `|:--:|` under a header row.
fn table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let trimmed = trimmed.trim_matches('|');
    if trimmed.is_empty() {
        return false;
    }
    trimmed.split('|').all(|cell| {
        let cell = cell.trim();
        !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':')
    }) && trimmed.contains('-')
}

fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

/// `- item` / `1. item` / `- [x] done` → `(depth, marker, text)`.
fn list_item(line: &str) -> Option<(usize, String, String)> {
    let indent = line.len() - line.trim_start().len();
    let depth = (indent / 2).min(4);
    let trimmed = line.trim_start();

    let (marker, rest) = if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        ("•".to_string(), rest)
    } else {
        let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let after = &trimmed[digits..];
        let punctuation = after.chars().next()?;
        if punctuation != '.' && punctuation != ')' {
            return None;
        }
        let rest = &after[1..];
        if !rest.starts_with(' ') {
            return None;
        }
        (format!("{}.", &trimmed[..digits]), rest)
    };

    // Task list: the marker itself carries the state.
    let rest = rest.trim_start();
    if let Some(task) = rest.strip_prefix("[ ] ") {
        return Some((depth, "☐".to_string(), task.to_string()));
    }
    if let Some(task) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        return Some((depth, "☑".to_string(), task.to_string()));
    }

    Some((depth, marker, rest.to_string()))
}

// ── Block rendering ─────────────────────────────────────────────────────────

fn render_block(block: Block, ctx: &mut RenderCtx<'_>, out: &mut Rendered) {
    let theme = ctx.theme.clone();

    match block {
        Block::Blank => out.lines.push(Line::raw("")),

        Block::Rule => {
            let width = ctx.width.saturating_sub(INDENT.len()).max(1);
            let style = Style::default().fg(theme.border_normal);
            emit_frags(
                vec![
                    Frag::plain(INDENT, Style::default()),
                    Frag::plain("─".repeat(width), style),
                ],
                ctx,
                out,
            );
        }

        Block::Heading { level, text } => {
            let color = if level <= 2 {
                theme.accent_primary
            } else {
                theme.accent_secondary
            };
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            let mut frags = vec![Frag::plain(INDENT, Style::default())];
            frags.extend(inline_frags(&text, style));
            emit_frags(resolve_frags(frags, ctx), ctx, out);
        }

        Block::Paragraph(text) => {
            let mut frags = vec![Frag::plain(INDENT, Style::default())];
            frags.extend(inline_frags(&text, Style::default().fg(theme.text_main)));
            emit_frags(resolve_frags(frags, ctx), ctx, out);
        }

        Block::Quote(lines) => {
            let gutter = Style::default().fg(theme.accent_primary);
            let body = Style::default()
                .fg(theme.text_muted)
                .add_modifier(Modifier::ITALIC);
            for line in lines {
                let mut frags = vec![
                    Frag::plain(INDENT, Style::default()),
                    Frag::plain("▏ ", gutter),
                ];
                frags.extend(inline_frags(&line, body));
                emit_frags(resolve_frags(frags, ctx), ctx, out);
            }
        }

        Block::ListItem {
            depth,
            marker,
            text,
        } => {
            let body = Style::default().fg(theme.text_main);
            let mut frags = vec![
                Frag::plain(INDENT, Style::default()),
                Frag::plain("  ".repeat(depth), Style::default()),
                Frag::plain(format!("{marker} "), Style::default().fg(theme.tool_bubble)),
            ];
            frags.extend(inline_frags(&text, body));
            emit_frags(resolve_frags(frags, ctx), ctx, out);
        }

        Block::Code { lang, lines } => render_code(&lang, &lines, ctx, out),

        Block::Table { header, rows } => render_table(&header, &rows, ctx, out),
    }
}

fn render_code(lang: &str, lines: &[String], ctx: &mut RenderCtx<'_>, out: &mut Rendered) {
    let theme = ctx.theme.clone();
    let gutter = Style::default().fg(theme.border_normal);
    let body = Style::default().fg(theme.text_main);

    // Every code row is `INDENT + "▏ "` (four cells) wide, so the body lines up
    // under the language label.
    let lead = || {
        vec![
            Cell {
                ch: ' ',
                style: gutter,
                link: None,
            },
            Cell {
                ch: ' ',
                style: gutter,
                link: None,
            },
            Cell {
                ch: '▏',
                style: gutter,
                link: None,
            },
            Cell {
                ch: ' ',
                style: gutter,
                link: None,
            },
        ]
    };

    if !lang.is_empty() {
        emit_frags(
            vec![
                Frag::plain(INDENT, Style::default()),
                Frag::plain("▏ ", gutter),
                Frag::plain(
                    lang.to_string(),
                    Style::default()
                        .fg(theme.text_muted)
                        .add_modifier(Modifier::ITALIC),
                ),
            ],
            ctx,
            out,
        );
    }

    let code_width = ctx.width.saturating_sub(INDENT.len() + 2).max(1);
    for line in lines {
        let cells = to_cells(&[Frag::plain(line.clone(), body)]);
        let (rows, truncated) = wrap_cells(&cells, code_width, CODE_MAX_ROWS);
        for row in rows {
            let mut all = lead();
            let mut row = row;
            all.append(&mut row);
            emit_row(out, all);
        }
        if truncated {
            let mut all = lead();
            all.push(Cell {
                ch: '…',
                style: Style::default().fg(theme.text_muted),
                link: None,
            });
            emit_row(out, all);
        }
    }
}

fn render_table(
    header: &[String],
    rows: &[Vec<String>],
    ctx: &mut RenderCtx<'_>,
    out: &mut Rendered,
) {
    let theme = ctx.theme.clone();
    let border = Style::default().fg(theme.border_normal);
    let head_style = Style::default()
        .fg(theme.text_main)
        .add_modifier(Modifier::BOLD);
    let cell_style = Style::default().fg(theme.text_main);

    let columns = header
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0));
    if columns == 0 {
        return;
    }

    // Markup is stripped for tables: aligning styled spans would cost far more
    // than the emphasis is worth in a terminal preview.
    let plain = |s: &str| -> String {
        inline_frags(s, cell_style)
            .iter()
            .map(|f| f.text.as_str())
            .collect::<String>()
    };

    let header_cells: Vec<String> = (0..columns)
        .map(|c| header.get(c).map(|s| plain(s)).unwrap_or_default())
        .collect();
    let body_cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns)
                .map(|c| row.get(c).map(|s| plain(s)).unwrap_or_default())
                .collect()
        })
        .collect();

    let mut widths: Vec<usize> = (0..columns)
        .map(|c| {
            let mut w = text_width(&header_cells[c]);
            for row in &body_cells {
                w = w.max(text_width(&row[c]));
            }
            w.min(TABLE_CELL_MAX)
        })
        .collect();
    // The indent and the `│ ` … ` │` frame cost `2 + 2 * columns + 1`.
    let frame = INDENT.len() + 2 * columns + 1;
    let available = ctx.width.saturating_sub(frame);

    // Too wide: fall back to unpadded rows that wrap like ordinary text.
    if widths.iter().sum::<usize>() > available {
        for row in std::iter::once(&header_cells).chain(body_cells.iter()) {
            let text = row.join(" │ ");
            let mut frags = vec![Frag::plain(INDENT, Style::default())];
            frags.push(Frag::plain(text, cell_style));
            emit_frags(frags, ctx, out);
        }
        return;
    }

    // Shrink the widest column until the table fits.
    while widths.iter().sum::<usize>() > available {
        let Some(widest) = (0..columns).max_by_key(|c| widths[*c]) else {
            break;
        };
        if widths[widest] <= 1 {
            break;
        }
        widths[widest] -= 1;
    }

    emit_table_row(&header_cells, head_style, &widths, ctx, out);
    let mut separator = vec![
        Frag::plain(INDENT, Style::default()),
        Frag::plain("├", border),
    ];
    for (c, w) in widths.iter().enumerate() {
        if c > 0 {
            separator.push(Frag::plain("┼", border));
        }
        separator.push(Frag::plain("─".repeat(w + 2), border));
    }
    separator.push(Frag::plain("┤", border));
    emit_frags(separator, ctx, out);

    for row in &body_cells {
        emit_table_row(row, cell_style, &widths, ctx, out);
    }
}

/// Emits one padded table row.
fn emit_table_row(
    cells: &[String],
    style: Style,
    widths: &[usize],
    ctx: &mut RenderCtx<'_>,
    out: &mut Rendered,
) {
    let border = Style::default().fg(ctx.theme.border_normal);
    let mut frags = vec![
        Frag::plain(INDENT, Style::default()),
        Frag::plain("│ ", border),
    ];
    for (c, cell) in cells.iter().enumerate() {
        if c > 0 {
            frags.push(Frag::plain(" │ ", border));
        }
        frags.push(Frag::plain(pad_cell(cell, widths[c]), style));
    }
    frags.push(Frag::plain(" │", border));
    emit_frags(frags, ctx, out);
}

/// Pads (or truncates) a table cell to `width` display columns.
fn pad_cell(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

// ── Inline parsing ──────────────────────────────────────────────────────────

/// Parses inline markup into fragments. Unclosed delimiters stay literal, which
/// is what keeps the streaming tail stable.
fn inline_frags(text: &str, base: Style) -> Vec<Frag> {
    let mut out: Vec<Frag> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while i < text.len() {
        let rest = &text[i..];

        // Backslash escape.
        if let Some(after) = rest.strip_prefix('\\') {
            if let Some(ch) = after.chars().next() {
                buf.push(ch);
                i += 1 + ch.len_utf8();
                continue;
            }
        }

        // Inline code (`path`), possibly a clickable file.
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                let content = &after[..end];
                flush(&mut buf, &mut out, base);
                if content.trim().is_empty() {
                    out.push(Frag::plain(content.to_string(), base));
                } else {
                    out.push(Frag::code(content.to_string(), base, content.trim()));
                }
                i += 1 + end + 1;
                continue;
            }
        }

        // Strong and strikethrough (two-character delimiters).
        if let Some((open, close, modifier)) = two_char_delim(rest) {
            if let Some(end) = rest[open.len()..].find(close) {
                let inner = &rest[open.len()..open.len() + end];
                if !inner.is_empty() {
                    flush(&mut buf, &mut out, base);
                    out.extend(inline_frags(inner, base.add_modifier(modifier)));
                    i += open.len() + end + close.len();
                    continue;
                }
            }
        }

        // Emphasis (single character).
        if let Some(delim) = rest.chars().next().filter(|c| *c == '*' || *c == '_') {
            let after = &rest[delim.len_utf8()..];
            let opens = after.chars().next().is_some_and(|c| !c.is_whitespace());
            if opens {
                if let Some(end) = after.find(delim) {
                    let inner = &after[..end];
                    if !inner.is_empty() && !inner.ends_with(char::is_whitespace) {
                        flush(&mut buf, &mut out, base);
                        out.extend(inline_frags(inner, base.add_modifier(Modifier::ITALIC)));
                        i += delim.len_utf8() + end + delim.len_utf8();
                        continue;
                    }
                }
            }
        }

        // Image: `![alt](target)`.
        if rest.starts_with("![") {
            if let Some((alt, target, len)) = parse_link(&rest[1..]) {
                flush(&mut buf, &mut out, base);
                out.push(Frag::with_target(
                    format!("🖼 {alt}"),
                    base.add_modifier(Modifier::ITALIC),
                    target,
                ));
                i += 1 + len;
                continue;
            }
        }

        // Link: `[text](target)`.
        if rest.starts_with('[') {
            if let Some((label, target, len)) = parse_link(rest) {
                flush(&mut buf, &mut out, base);
                // The label keeps its own inline markup, but the whole run
                // points at the target.
                for mut frag in inline_frags(label, base) {
                    frag.target = Some(target.to_string());
                    out.push(frag);
                }
                i += len;
                continue;
            }
        }

        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }

    flush(&mut buf, &mut out, base);
    out
}

fn flush(buf: &mut String, out: &mut Vec<Frag>, style: Style) {
    if !buf.is_empty() {
        out.push(Frag::plain(std::mem::take(buf), style));
    }
}

/// `(open, close, modifier)` for the two-character span delimiters.
fn two_char_delim(rest: &str) -> Option<(&'static str, &'static str, Modifier)> {
    if rest.starts_with("**") {
        Some(("**", "**", Modifier::BOLD))
    } else if rest.starts_with("__") {
        Some(("__", "__", Modifier::BOLD))
    } else if rest.starts_with("~~") {
        Some(("~~", "~~", Modifier::CROSSED_OUT))
    } else {
        None
    }
}

/// `[label](target)` → `(label, target, consumed_bytes)`. The `[` is included.
fn parse_link(s: &str) -> Option<(&str, &str, usize)> {
    let close = s.find("](")?;
    let label = &s[1..close];
    let after = &s[close + 2..];
    let end = after.find(')')?;
    let raw = after[..end].trim();
    // An optional title follows the target: `(path "Title")`.
    let target = raw.split_whitespace().next().unwrap_or(raw);
    if target.is_empty() {
        return None;
    }
    Some((label, target, close + 2 + end + 1))
}

// ── Link resolution ─────────────────────────────────────────────────────────

/// Turns pending targets into resolved links and finds bare URLs / paths.
fn resolve_frags(frags: Vec<Frag>, ctx: &mut RenderCtx<'_>) -> Vec<Frag> {
    let theme = ctx.theme.clone();
    let link_style = Style::default()
        .fg(theme.user_bubble)
        .add_modifier(Modifier::UNDERLINED);
    let code_style = Style::default()
        .fg(theme.accent_secondary)
        .bg(theme.surface);
    let mut out = Vec::new();

    for frag in frags {
        // Inline code keeps its own chrome whether or not it resolves.
        let style = if frag.code {
            frag.style.patch(code_style)
        } else {
            frag.style
        };

        if let Some(target) = frag.target.clone() {
            match classify(&target, ctx.roots, ctx.cache) {
                Some(link) => out.push(Frag {
                    text: frag.text,
                    style: style.patch(link_style),
                    target: None,
                    link: Some(link),
                    code: false,
                }),
                // Not a real target: keep the text, drop the link affordance.
                None => out.push(Frag {
                    text: frag.text,
                    style,
                    target: None,
                    link: None,
                    code: false,
                }),
            }
        } else if frag.link.is_none() {
            out.extend(linkify_plain(Frag { style, ..frag }, ctx));
        } else {
            out.push(Frag { style, ..frag });
        }
    }
    out
}

/// Splits a plain fragment on whitespace, linking any token that resolves.
fn linkify_plain(frag: Frag, ctx: &mut RenderCtx<'_>) -> Vec<Frag> {
    let theme = ctx.theme.clone();
    let mut out = Vec::new();
    let mut plain = String::new();

    for segment in segments(&frag.text) {
        if segment.chars().all(char::is_whitespace) {
            plain.push_str(segment);
            continue;
        }
        match classify(segment, ctx.roots, ctx.cache) {
            Some(link) => {
                if !plain.is_empty() {
                    out.push(Frag::plain(std::mem::take(&mut plain), frag.style));
                }
                let style = frag.style.patch(
                    Style::default()
                        .fg(theme.user_bubble)
                        .add_modifier(Modifier::UNDERLINED),
                );
                out.push(Frag {
                    text: segment.to_string(),
                    style,
                    target: None,
                    link: Some(link),
                    code: false,
                });
            }
            None => plain.push_str(segment),
        }
    }
    if !plain.is_empty() {
        out.push(Frag::plain(plain, frag.style));
    }
    out
}

/// Splits text into alternating whitespace / non-whitespace runs.
fn segments(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut mode: Option<bool> = None;
    for (i, ch) in text.char_indices() {
        let ws = ch.is_whitespace();
        match mode {
            None => {
                mode = Some(ws);
                start = i;
            }
            Some(prev) if prev != ws => {
                out.push(&text[start..i]);
                start = i;
                mode = Some(ws);
            }
            _ => {}
        }
    }
    if mode.is_some() {
        out.push(&text[start..]);
    }
    out
}

// ── Wrapping and line building ──────────────────────────────────────────────

fn to_cells(frags: &[Frag]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for frag in frags {
        for ch in frag.text.chars() {
            cells.push(Cell {
                ch,
                style: frag.style,
                link: frag.link.clone(),
            });
        }
    }
    cells
}

/// Wraps cells to `width`, never splitting a wide character across rows.
/// Returns the rows and whether content was dropped at `max_rows`.
fn wrap_cells(cells: &[Cell], width: usize, max_rows: usize) -> (Vec<Vec<Cell>>, bool) {
    let width = width.max(1);
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;

    for cell in cells {
        let w = char_width(cell.ch);
        if used + w > width && !row.is_empty() {
            if rows.len() + 1 >= max_rows {
                truncated = true;
                break;
            }
            rows.push(std::mem::take(&mut row));
            used = 0;
        }
        used += w;
        row.push(cell.clone());
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(Vec::new());
    }
    (rows, truncated)
}

fn emit_frags(frags: Vec<Frag>, ctx: &mut RenderCtx<'_>, out: &mut Rendered) {
    let cells = to_cells(&frags);
    let (rows, _) = wrap_cells(&cells, ctx.width, usize::MAX);
    for row in rows {
        emit_row(out, row);
    }
}

/// Converts one row of cells into a `Line`, recording link hitboxes.
fn emit_row(out: &mut Rendered, row: Vec<Cell>) {
    let line_index = out.lines.len();

    // Merge adjacent cells that share a style into spans.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style: Option<Style> = None;
    for cell in &row {
        match style {
            Some(s) if s == cell.style => text.push(cell.ch),
            Some(s) => {
                spans.push(Span::styled(std::mem::take(&mut text), s));
                text.push(cell.ch);
                style = Some(cell.style);
            }
            None => {
                text.push(cell.ch);
                style = Some(cell.style);
            }
        }
    }
    if let Some(s) = style {
        spans.push(Span::styled(text, s));
    }
    out.lines.push(Line::from(spans));

    // Link ranges: consecutive cells sharing a target become one hitbox.
    let mut columns = Vec::with_capacity(row.len() + 1);
    let mut col = 0u16;
    for cell in &row {
        columns.push(col);
        col += char_width(cell.ch) as u16;
    }
    columns.push(col);

    let mut i = 0;
    while i < row.len() {
        let Some(link) = row[i].link.clone() else {
            i += 1;
            continue;
        };
        let mut j = i;
        while j + 1 < row.len() && row[j + 1].link.as_ref() == Some(&link) {
            j += 1;
        }
        out.links.push(LinkSpan {
            line: line_index,
            col_start: columns[i],
            col_end: columns[j + 1],
            target: link,
        });
        i = j + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;
    use tempfile::tempdir;

    fn render_str(text: &str, width: usize) -> Rendered {
        let theme = Theme::default();
        let dir = tempdir().unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let mut cache = LinkCache::new();
        let mut ctx = RenderCtx {
            theme: &theme,
            width,
            roots: &roots,
            cache: &mut cache,
        };
        render(text, &mut ctx)
    }

    fn plain_text(rendered: &Rendered) -> Vec<String> {
        rendered
            .lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn has_modifier(rendered: &Rendered, modifier: Modifier) -> bool {
        rendered
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.add_modifier.contains(modifier))
    }

    #[test]
    fn headings_render_without_hashes() {
        let out = render_str("# Title\n## Sub\ntext", 40);
        let text = plain_text(&out);
        assert!(text[0].contains("Title"));
        assert!(!text[0].contains('#'), "{:?}", text[0]);
        assert!(text[1].contains("Sub"));
        assert!(has_modifier(&out, Modifier::BOLD));
    }

    #[test]
    fn emphasis_and_code() {
        let out = render_str("a **bold** and *italic* and `code`", 80);
        let text = plain_text(&out).join("\n");
        // Markup is consumed, content survives.
        assert!(!text.contains("**"), "{text:?}");
        assert!(!text.contains('`'), "{text:?}");
        assert!(text.contains("bold") && text.contains("italic") && text.contains("code"));
        assert!(has_modifier(&out, Modifier::BOLD));
        assert!(has_modifier(&out, Modifier::ITALIC));
    }

    #[test]
    fn unclosed_delimiters_stay_literal() {
        // The streaming tail: a half-typed emphasis must not swallow the line.
        let out = render_str("hello **wor", 80);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("hello **wor"), "{text:?}");
    }

    #[test]
    fn fenced_code_is_not_parsed_as_markdown() {
        let out = render_str("```rust\nlet x = **2**;\n```", 80);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("let x = **2**;"), "{text:?}");
        assert!(text.contains("rust"), "language label: {text:?}");
    }

    #[test]
    fn unclosed_fence_still_renders_as_code() {
        let out = render_str("```\nstill code\nand more", 80);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("still code"));
        assert!(text.contains("and more"));
    }

    #[test]
    fn lists_and_tasks() {
        let out = render_str("- one\n- [x] done\n1. first\n  - nested", 40);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("• one"), "{text:?}");
        assert!(text.contains("☑ done"), "{text:?}");
        assert!(text.contains("1. first"), "{text:?}");
        assert!(text.contains("    • nested"), "nesting by indent: {text:?}");
    }

    #[test]
    fn rules_and_quotes() {
        let out = render_str("before\n---\n> quoted", 20);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("─"), "{text:?}");
        assert!(text.contains("▏ quoted"), "{text:?}");
    }

    #[test]
    fn tables_align_columns() {
        let out = render_str("| a | b |\n|---|---|\n| 1 | 22 |", 60);
        let text = plain_text(&out);
        assert!(text.iter().any(|l| l.contains('│')), "{text:?}");
        assert!(
            text.iter().any(|l| l.contains('─') && l.contains('┼')),
            "{text:?}"
        );
        // Columns are padded to a common width.
        let rows: Vec<&String> = text.iter().filter(|l| l.contains('│')).collect();
        let widths: Vec<usize> = rows.iter().map(|l| text_width(l)).collect();
        assert!(widths.windows(2).all(|w| w[0] == w[1]), "{widths:?}");
    }

    #[test]
    fn narrow_table_degrades_instead_of_overflowing() {
        let out = render_str("| col one | col two |\n|---|---|\n| a | b |", 12);
        for line in plain_text(&out) {
            assert!(text_width(&line) <= 12, "line overflows: {line:?}");
        }
    }

    #[test]
    fn wrapping_respects_width_and_wide_chars() {
        let out = render_str("一二三四五六七八九十", 8);
        for line in plain_text(&out) {
            assert!(text_width(&line) <= 8, "line overflows: {line:?}");
        }
        // No wide character is split across rows: reassembling rows rebuilds
        // the original text.
        let joined: String = plain_text(&out)
            .iter()
            .map(|l| l.trim_start().to_string())
            .collect();
        assert_eq!(joined, "一二三四五六七八九十");
    }

    #[test]
    fn every_rendered_line_is_one_screen_row() {
        // Wrapping happens here, so this is the invariant the scroll maths and
        // the link hitboxes both rest on.
        let long = "x".repeat(200);
        let out = render_str(&long, 30);
        for line in plain_text(&out) {
            assert!(text_width(&line) <= 30, "line overflows: {line:?}");
        }
        // 200 cells plus the two-cell indent, wrapped at 30.
        assert_eq!(out.lines.len(), 202usize.div_ceil(30));
        // Rows reassemble the original text exactly.
        let joined: String = plain_text(&out)
            .iter()
            .map(|l| l.trim_start().to_string())
            .collect();
        assert_eq!(joined, long);
    }

    #[test]
    fn inline_code_paths_become_links() {
        let theme = Theme::default();
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/app.rs"), "x").unwrap();
        let roots = vec![dir.path().to_path_buf()];
        let mut cache = LinkCache::new();
        let mut ctx = RenderCtx {
            theme: &theme,
            width: 80,
            roots: &roots,
            cache: &mut cache,
        };

        let out = render("see `src/app.rs:12` now", &mut ctx);
        assert_eq!(out.links.len(), 1, "{:?}", out.links);
        assert_eq!(
            out.links[0].target,
            LinkTarget::File(dir.path().join("src/app.rs"))
        );
        // The hitbox is inside the rendered line and excludes the `:12`.
        let line = &out.lines[out.links[0].line];
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("src/app.rs"), "{text:?}");
    }

    #[test]
    fn markdown_link_and_bare_url_are_clickable() {
        let out = render_str(
            "see [docs](https://example.com) or https://rust-lang.org here",
            120,
        );
        let targets: Vec<_> = out.links.iter().map(|l| l.target.clone()).collect();
        assert!(
            targets.contains(&LinkTarget::Url("https://example.com".into())),
            "{targets:?}"
        );
        assert!(
            targets.contains(&LinkTarget::Url("https://rust-lang.org".into())),
            "{targets:?}"
        );
    }

    #[test]
    fn nonexistent_paths_are_not_links() {
        let out = render_str("ratio is a/b and `missing.rs` too", 80);
        assert!(out.links.is_empty(), "{:?}", out.links);
    }

    #[test]
    fn escaped_markup_is_literal() {
        let out = render_str(r"not \*italic\* and not \`code\`", 80);
        let text = plain_text(&out).join("\n");
        assert!(text.contains("not *italic*"), "{text:?}");
        assert!(!has_modifier(&out, Modifier::ITALIC));
    }
}
