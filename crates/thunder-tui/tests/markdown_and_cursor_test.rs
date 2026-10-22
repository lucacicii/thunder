use ratatui::{backend::TestBackend, Terminal};
use thunder_tui::{ui, App, TimelineItem};

fn frame_text(terminal: &Terminal<TestBackend>) -> String {
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
fn a_markdown_fenced_document_is_rendered_as_markdown_not_raw_source() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.timeline.push(TimelineItem::Assistant {
        content: "\
下面是一段示例：

````markdown
# 设计文档

| 术语 | 说明 |
| :--- | :--- |
| 任务 | 最小调度单元 |

### 校验清单

- [x] 已完成项
````

如果需要，我可以写入文件。"
            .to_string(),
        reasoning: String::new(),
        streaming: false,
    });

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();
    let text = frame_text(&terminal);

    assert!(
        !text.contains("````markdown"),
        "the markdown fence must not be printed as source; frame was:\n{text}"
    );
    assert!(
        !text.contains("###校验清单"),
        "a heading inside the fence must render as a heading, not literal hashes"
    );
    assert!(
        !text.contains("|:---|"),
        "a table inside the fence must render as a table, not raw pipes"
    );
    assert!(
        text.contains("校验清单"),
        "the heading text must be visible"
    );
    assert!(text.contains("最小调度单元"), "table cells must be visible");
    assert!(
        text.contains("如果需要"),
        "prose outside the fence must be visible"
    );
}

#[test]
fn the_prompt_shows_a_cursor_one_cell_past_the_text() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.input = "hello".to_string();

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // Composer is the last chunk (4 rows), so its inner row is `height - 3`;
    // the text starts one cell in, past the left border.
    let area = terminal.size().unwrap();
    let expected = ratatui::layout::Position::new(1 + "hello".len() as u16, area.height - 3);

    assert_eq!(
        terminal
            .get_cursor_position()
            .expect("a cursor must be placed"),
        expected,
        "caret sits one cell past the typed text"
    );
}

#[test]
fn the_cursor_is_hidden_while_a_picker_modal_is_open() {
    let mut app = App::new("test/model");
    app.timeline.clear();
    app.open_tools_picker();

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| ui::render(f, &mut app)).unwrap();

    // ratatui reports the hidden cursor at the origin with a flag; assert the
    // visible-cursor helper refuses to place one behind a dialog.
    assert!(app.picker.is_open);
    let composer_area = ratatui::layout::Rect::new(0, 16, 80, 4);
    assert_eq!(thunder_tui::ui::compose_cursor(&app, composer_area), None);
}
