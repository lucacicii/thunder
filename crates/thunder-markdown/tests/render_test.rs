use thunder_markdown::{parse_inline, render_markdown_default};

#[test]
fn renders_headings_paragraphs_and_inline_styles() {
    let md = "# Title\n\nBody with **bold** and `code`.\n\n## Subtitle";
    let lines = render_markdown_default(md, 80);
    assert!(!lines.is_empty());

    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("Title"));
    assert!(full.contains("Body with"));
    assert!(full.contains("bold"));
    assert!(full.contains("Subtitle"));
}

#[test]
fn renders_fenced_code_with_language() {
    let md = "```rust\nfn main() {}\n```";
    let lines = render_markdown_default(md, 80);
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("fn main"));
}

#[test]
fn streaming_unclosed_code_fence_is_closed_synthetically() {
    let streaming = "```python\ndef test():\n    pass";
    let lines = render_markdown_default(streaming, 80);
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("def test"));
}

#[test]
fn renders_gfm_alerts_and_task_lists() {
    let md = "> [!TIP]\n> Verify with cargo test.\n\n- [x] Done\n- [ ] Pending";
    let lines = render_markdown_default(md, 80);
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("Verify with cargo test"));
    assert!(full.contains("Done"));
    assert!(full.contains("Pending"));
}

#[test]
fn renders_tables() {
    let table = "| Name | Role |\n|---|---|\n| Alice | Dev |\n| Bob | Ops |";
    let lines = render_markdown_default(table, 80);
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("Alice"));
    assert!(full.contains("Role"));
}

#[test]
fn hash_and_preprocessor_lines_never_hang() {
    let tricky = "#include <stdio.h>\n#define MAX_VAL 100\n#!/bin/bash\n#tag\nRegular paragraph.";
    let lines = render_markdown_default(tricky, 80);
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("#include <stdio.h>"));
}

#[test]
fn inline_parser_keeps_snake_case_identifiers_intact() {
    let spans = parse_inline("const MAX_VAL = 100");
    let text: String = spans.iter().map(|s| s.content.to_string()).collect();
    assert_eq!(text, "const MAX_VAL = 100");
}
