#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalloutKind {
    Note,
    Tip,
    Warning,
    Important,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    Heading {
        level: u8,
        text: String,
    },
    CodeFence {
        lang: String,
        lines: Vec<String>,
        is_closed: bool,
    },
    Callout {
        kind: CalloutKind,
        title: String,
        lines: Vec<String>,
    },
    Blockquote(Vec<String>),
    TaskItem {
        checked: bool,
        text: String,
    },
    ListItem {
        indent: usize,
        bullet: String,
        text: String,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    ThematicBreak,
    Paragraph(String),
}

/// Parse source Markdown into structured block-level elements.
///
/// Designed to be lenient with streaming LLM inputs (e.g. unclosed code fences).
pub fn parse_blocks(source: &str) -> Vec<MarkdownBlock> {
    let raw_lines: Vec<&str> = source.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;

    while i < raw_lines.len() {
        let line = raw_lines[i].trim_end();

        // 1. Code Fence (```lang ... ```)
        if let Some((fence_char, lang)) = parse_fence_start(line) {
            let mut body = Vec::new();
            let mut is_closed = false;
            i += 1;
            while i < raw_lines.len() {
                let candidate = raw_lines[i].trim_end();
                if is_fence_end(candidate, fence_char) {
                    is_closed = true;
                    i += 1;
                    break;
                }
                body.push(raw_lines[i].to_string());
                i += 1;
            }
            blocks.push(MarkdownBlock::CodeFence {
                lang,
                lines: body,
                is_closed,
            });
            continue;
        }

        // 2. Headings (# H1, ## H2, etc.)
        if let Some((level, text)) = parse_heading(line) {
            blocks.push(MarkdownBlock::Heading { level, text });
            i += 1;
            continue;
        }

        // 3. Thematic break (---, ***, ___)
        if is_thematic_break(line) {
            blocks.push(MarkdownBlock::ThematicBreak);
            i += 1;
            continue;
        }

        // 4. Blockquotes & Callouts (> [!NOTE], > ...)
        if line.trim_start().starts_with('>') {
            let mut q_lines = Vec::new();
            while i < raw_lines.len() {
                let candidate = raw_lines[i].trim_end();
                let trimmed_cand = candidate.trim_start();
                if let Some(rest) = trimmed_cand.strip_prefix('>') {
                    q_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                    i += 1;
                } else {
                    break;
                }
            }

            // Check if callout alert: > [!TIP]
            if let Some((kind, title)) =
                parse_callout_header(q_lines.first().map(String::as_str).unwrap_or(""))
            {
                let body = if q_lines.len() > 1 {
                    q_lines[1..].to_vec()
                } else {
                    Vec::new()
                };
                blocks.push(MarkdownBlock::Callout {
                    kind,
                    title,
                    lines: body,
                });
            } else {
                blocks.push(MarkdownBlock::Blockquote(q_lines));
            }
            continue;
        }

        // 5. Task List Item (- [x] or - [ ])
        if let Some((checked, text)) = parse_task_item(line) {
            blocks.push(MarkdownBlock::TaskItem { checked, text });
            i += 1;
            continue;
        }

        // 6. List Item (- item, * item, 1. item)
        if let Some((indent, bullet, text)) = parse_list_item(line) {
            blocks.push(MarkdownBlock::ListItem {
                indent,
                bullet,
                text,
            });
            i += 1;
            continue;
        }

        // 7. Table (| Header | Header |)
        if line.contains('|') && i + 1 < raw_lines.len() && is_table_delimiter(raw_lines[i + 1]) {
            let headers = split_table_row(line);
            i += 2; // skip header + delimiter
            let mut rows = Vec::new();
            while i < raw_lines.len()
                && raw_lines[i].contains('|')
                && !raw_lines[i].trim().is_empty()
            {
                rows.push(split_table_row(raw_lines[i]));
                i += 1;
            }
            blocks.push(MarkdownBlock::Table { headers, rows });
            continue;
        }

        // 8. Blank line
        if line.trim().is_empty() {
            i += 1;
            continue;
        }

        // 9. Standard Paragraph (accumulate adjacent lines)
        let mut para_lines = Vec::new();
        while i < raw_lines.len() {
            let cur = raw_lines[i].trim_end();
            if cur.trim().is_empty()
                || cur.starts_with('#')
                || cur.starts_with("```")
                || cur.trim_start().starts_with('>')
                || cur.trim_start().starts_with("- [")
                || cur.trim_start().starts_with("- ")
                || cur.trim_start().starts_with("* ")
                || is_thematic_break(cur)
            {
                break;
            }
            para_lines.push(cur);
            i += 1;
        }
        if !para_lines.is_empty() {
            blocks.push(MarkdownBlock::Paragraph(para_lines.join(" ")));
        }
    }

    blocks
}

fn parse_fence_start(line: &str) -> Option<(char, String)> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("```") {
        let lang = trimmed.trim_start_matches('`').trim().to_string();
        Some(('`', lang))
    } else if trimmed.starts_with("~~~") {
        let lang = trimmed.trim_start_matches('~').trim().to_string();
        Some(('~', lang))
    } else {
        None
    }
}

fn is_fence_end(line: &str, fence_char: char) -> bool {
    let trimmed = line.trim();
    if fence_char == '`' && trimmed.starts_with("```") {
        trimmed.chars().all(|c| c == '`')
    } else if fence_char == '~' && trimmed.starts_with("~~~") {
        trimmed.chars().all(|c| c == '~')
    } else {
        false
    }
}

fn parse_heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        if rest.starts_with(' ') || rest.is_empty() {
            return Some((hashes as u8, rest.trim().to_string()));
        }
    }
    None
}

fn is_thematic_break(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    (trimmed.chars().all(|c| c == '-')
        || trimmed.chars().all(|c| c == '*')
        || trimmed.chars().all(|c| c == '_'))
        && !trimmed.contains(' ')
}

fn parse_callout_header(first_line: &str) -> Option<(CalloutKind, String)> {
    let trimmed = first_line.trim();
    if let Some(rest) = trimmed.strip_prefix("[!") {
        if let Some((tag, title)) = rest.split_once(']') {
            let kind = match tag.to_uppercase().as_str() {
                "TIP" => CalloutKind::Tip,
                "NOTE" | "INFO" => CalloutKind::Note,
                "WARNING" | "WARN" | "CAUTION" => CalloutKind::Warning,
                "IMPORTANT" | "DANGER" | "ERROR" => CalloutKind::Important,
                _ => return None,
            };
            let custom_title = title.trim();
            let title_str = if custom_title.is_empty() {
                format!("{tag:?}")
            } else {
                custom_title.to_string()
            };
            return Some((kind, title_str));
        }
    }
    None
}

fn parse_task_item(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- [x] ") {
        Some((true, rest.trim().to_string()))
    } else if let Some(rest) = trimmed.strip_prefix("- [X] ") {
        Some((true, rest.trim().to_string()))
    } else {
        trimmed
            .strip_prefix("- [ ] ")
            .map(|rest| (false, rest.trim().to_string()))
    }
}

fn parse_list_item(line: &str) -> Option<(usize, String, String)> {
    let indent = line.chars().take_while(|c| c.is_whitespace()).count();
    let trimmed = line.trim_start();

    if let Some(rest) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return Some((indent, "•".to_string(), rest.trim().to_string()));
    }
    if let Some((num, rest)) = trimmed.split_once(". ") {
        if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
            return Some((indent, format!("{num}."), rest.trim().to_string()));
        }
    }
    None
}

fn is_table_delimiter(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.contains('-')
}

fn split_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}
