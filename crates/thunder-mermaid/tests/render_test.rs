use thunder_mermaid::{render, RenderError};

#[test]
fn renders_a_small_flowchart_as_box_drawing() {
    let out = render("flowchart LR\n  A[Client] --> B[Server]", 80).expect("renders");
    assert!(out.contains("Client"), "node labels must survive:\n{out}");
    assert!(out.contains("Server"));
    assert!(
        out.chars().any(|c| "─│┌┐└┘".contains(c)),
        "box drawing expected:\n{out}"
    );
}

#[test]
fn renders_a_state_diagram() {
    let src = "stateDiagram-v2\n  [*] --> PENDING\n  PENDING --> RUNNING\n  RUNNING --> DONE\n  DONE --> [*]";
    let out = render(src, 100).expect("renders");
    assert!(out.contains("PENDING"));
    assert!(out.contains("RUNNING"));
}

#[test]
fn a_diagram_wider_than_the_budget_is_refused_instead_of_truncated() {
    let src = "flowchart LR\n  A[aaaaaaaaaaaaaaaaaaaa] --> B[bbbbbbbbbbbbbbbbbbbb] --> C[cccccccccccccccccccc]";
    assert!(matches!(render(src, 20), Err(RenderError::TooWide)));
}

#[test]
fn unsupported_syntax_is_refused() {
    assert!(render("not a diagram at all", 80).is_err());
}
