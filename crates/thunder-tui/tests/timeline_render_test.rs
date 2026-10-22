use ratatui::{backend::TestBackend, Terminal};
use thunder_tui::{ui, App, TimelineItem};

/// Flatten the frame into one string.
///
/// Wide (CJK) glyphs occupy two cells and leave the following cell blank, so
/// spaces are stripped: assertions then match the text regardless of scripts.
fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>()
        .replace(' ', "")
}

#[test]
fn timeline_renders_user_and_assistant_markdown_into_the_frame() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.timeline.push(TimelineItem::User {
        text: "请解释一下微内核架构".to_string(),
    });
    app.timeline.push(TimelineItem::Assistant {
        content: "## 微内核\n\n核心只认两个接缝：\n\n```rust\nfn seal() {}\n```\n".to_string(),
        reasoning: String::new(),
        streaming: false,
    });

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    let text = buffer_text(&terminal);
    assert!(
        text.contains("请解释一下微内核架构"),
        "user message must be visible; buffer was:\n{text}"
    );
    assert!(
        text.contains("微内核"),
        "assistant markdown must be visible; buffer was:\n{text}"
    );
    assert!(
        text.contains("seal"),
        "fenced code must be visible; buffer was:\n{text}"
    );
}

#[test]
fn timeline_still_renders_when_scroll_offset_is_stale_and_large() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.timeline.push(TimelineItem::User {
        text: "hello".to_string(),
    });
    // Simulate a scroll position left behind by an earlier, longer transcript.
    app.scroll_back = 5000;

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    let text = buffer_text(&terminal);
    assert!(
        text.contains("hello"),
        "an out-of-range scroll offset must not blank the transcript; buffer was:\n{text}"
    );
}

#[test]
fn streaming_reasoning_only_turn_is_visible() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.timeline.push(TimelineItem::Assistant {
        content: String::new(),
        reasoning: "thinking about the kernel".to_string(),
        streaming: true,
    });

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    let text = buffer_text(&terminal);
    assert!(
        text.contains("thinkingaboutthekernel"),
        "reasoning stream must be visible; buffer was:\n{text}"
    );
}
