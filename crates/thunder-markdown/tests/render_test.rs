use thunder_markdown::{parse_blocks, render_markdown_default, CalloutKind, MarkdownBlock};

#[test]
fn renders_headers_and_paragraphs() {
    let md = "# Title\n\nParagraph text with **bold** and `code`.\n\n## Subtitle";
    let lines = render_markdown_default(md, 80);
    assert!(!lines.is_empty());

    let h1_line = lines[0].to_string();
    assert!(h1_line.contains("Title"));
}

#[test]
fn renders_code_cards_with_language_badge() {
    let md = "```rust\nfn main() {\n    println!(\"Hello\");\n}\n```";
    let lines = render_markdown_default(md, 80);
    let full_text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(full_text.contains("[rust]"));
    assert!(full_text.contains("fn main"));
    assert!(full_text.contains("╭─"));
    assert!(full_text.contains("╰─"));
}

#[test]
fn streaming_unclosed_code_fence_is_lenient_and_does_not_break() {
    let streaming_md = "```python\ndef test():\n    pass";
    let lines = render_markdown_default(streaming_md, 80);
    let full_text: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(full_text.contains("[python]"));
    assert!(full_text.contains("def test"));
    assert!(full_text.contains("streaming"));
}

#[test]
fn parses_callouts_and_task_lists() {
    let md = "> [!TIP]\n> Use cargo test to verify.\n\n- [x] Task 1\n- [ ] Task 2";
    let blocks = parse_blocks(md);

    let has_tip = blocks.iter().any(|b| {
        matches!(
            b,
            MarkdownBlock::Callout {
                kind: CalloutKind::Tip,
                ..
            }
        )
    });
    assert!(has_tip);

    let has_checked = blocks
        .iter()
        .any(|b| matches!(b, MarkdownBlock::TaskItem { checked: true, .. }));
    let has_unchecked = blocks
        .iter()
        .any(|b| matches!(b, MarkdownBlock::TaskItem { checked: false, .. }));
    assert!(has_checked);
    assert!(has_unchecked);

    let lines = render_markdown_default(md, 80);
    let rendered = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("TIP"));
    assert!(rendered.contains("✔"));
    assert!(rendered.contains("○"));
}

#[test]
fn renders_tables_with_grid() {
    let table_md = "| Name | Role |\n|---|---|\n| Alice | Dev |\n| Bob | Ops |";
    let lines = render_markdown_default(table_md, 80);
    let rendered = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Alice"));
    assert!(rendered.contains("Role"));
    assert!(rendered.contains("│"));
    assert!(rendered.contains("┼"));
}

#[test]
fn parses_hash_and_preprocessor_lines_without_hanging() {
    let tricky_md = "\
#include <stdio.h>
#define MAX_VAL 100
#!/bin/bash
#tag_without_space
- [x]invalid_task_no_space
Regular paragraph text.";

    // Must finish in < 50ms without hanging in an infinite loop!
    let lines = render_markdown_default(tricky_md, 80);
    assert!(!lines.is_empty());
    let full: String = lines
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("#include <stdio.h>"));
    assert!(full.contains("#define MAX_VAL 100"));
}
