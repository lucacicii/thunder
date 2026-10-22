use thunder_markdown::render_markdown_default;

fn render(src: &str, w: usize) -> String {
    render_markdown_default(src, w)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_mermaid_fence_is_drawn_as_a_diagram() {
    let src = "## 架构\n\n```mermaid\nflowchart LR\n  A[Client] --> B[Server]\n```\n";
    let out = render(src, 120);

    assert!(
        !out.contains("flowchart LR"),
        "diagram source must be replaced by the drawing; got:\n{out}"
    );
    assert!(
        out.contains("▸ mermaid diagram"),
        "a caption marks the drawing:\n{out}"
    );
    assert!(
        out.contains("Client") && out.contains("Server"),
        "labels survive:\n{out}"
    );
    assert!(
        out.chars().any(|c| "─│┌┐└┘".contains(c)),
        "box drawing expected:\n{out}"
    );
    assert!(
        out.contains("架构"),
        "surrounding markdown still renders:\n{out}"
    );
}

#[test]
fn an_unrenderable_diagram_falls_back_to_its_source() {
    // Too wide for the budget: refuse and show the source rather than truncate.
    let src = "```mermaid\nflowchart LR\n  A[aaaaaaaaaaaaaaaaaaaa] --> B[bbbbbbbbbbbbbbbbbbbb] --> C[cccccccccccccccccccc]\n```\n";
    let out = render(src, 40);
    assert!(
        out.contains("flowchart LR"),
        "source must be visible:\n{out}"
    );
}

#[test]
fn a_non_mermaid_fence_is_untouched() {
    let src = "```rust\nfn main() {}\n```\n";
    let out = render(src, 100);
    assert!(out.contains("fn main"), "{out}");
    assert!(!out.contains("mermaid diagram"), "{out}");
}
