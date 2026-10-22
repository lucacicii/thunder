use thunder_tui::transcript_cache::TranscriptCache;
use thunder_tui::{App, TimelineItem};

fn assistant(body: &str, streaming: bool) -> TimelineItem {
    TimelineItem::Assistant {
        content: body.to_string(),
        reasoning: String::new(),
        streaming,
    }
}

#[test]
fn unchanged_items_are_not_rendered_again() {
    use std::cell::Cell;

    let timeline = vec![
        TimelineItem::User {
            text: "first".into(),
        },
        assistant("## one\n\nbody", false),
    ];

    let renders = Cell::new(0usize);
    let mut cache = TranscriptCache::default();
    let render = |item: &TimelineItem, _w: u16| {
        renders.set(renders.get() + 1);
        vec![ratatui::text::Line::from(match item {
            TimelineItem::User { text } => text.clone(),
            TimelineItem::Assistant { content, .. } => content.clone(),
            _ => String::new(),
        })]
    };

    cache.lines(&timeline, 80, render);
    assert_eq!(renders.get(), 2, "first frame renders every item");

    cache.lines(&timeline, 80, render);
    assert_eq!(
        renders.get(),
        2,
        "a second frame with identical content renders nothing"
    );
}

#[test]
fn only_the_growing_streaming_item_is_re_rendered() {
    use std::cell::Cell;

    let mut timeline = vec![
        TimelineItem::User {
            text: "question".into(),
        },
        assistant("done turn", false),
        assistant("partial", true),
    ];

    let renders = Cell::new(0usize);
    let mut cache = TranscriptCache::default();
    let render = |_item: &TimelineItem, _w: u16| {
        renders.set(renders.get() + 1);
        vec![ratatui::text::Line::from("x")]
    };

    cache.lines(&timeline, 80, render);
    assert_eq!(renders.get(), 3);

    if let TimelineItem::Assistant { content, .. } = timeline.last_mut().unwrap() {
        content.push_str(" more");
    }
    cache.lines(&timeline, 80, render);
    assert_eq!(
        renders.get(),
        4,
        "only the streaming tail may be re-parsed when a token arrives"
    );
}

#[test]
fn a_width_change_invalidates_every_cached_line() {
    use std::cell::Cell;

    let timeline = vec![assistant("hello world", false)];
    let renders = Cell::new(0usize);
    let mut cache = TranscriptCache::default();
    let render = |_item: &TimelineItem, _w: u16| {
        renders.set(renders.get() + 1);
        vec![ratatui::text::Line::from("x")]
    };

    cache.lines(&timeline, 80, render);
    cache.lines(&timeline, 40, render);
    assert_eq!(
        renders.get(),
        2,
        "wrapping is width-dependent, so a resize must re-render"
    );
}

#[test]
fn long_lines_are_pre_wrapped_to_fit_the_pane() {
    let timeline = vec![TimelineItem::User {
        text: "word ".repeat(60),
    }];
    let mut cache = TranscriptCache::default();
    let lines = cache.lines(&timeline, 40, |item, _w| {
        let text = match item {
            TimelineItem::User { text } => text.clone(),
            _ => String::new(),
        };
        vec![ratatui::text::Line::from(text)]
    });

    let max_width = lines.iter().map(|l| l.width()).max().unwrap_or(0);
    assert!(
        max_width <= 38,
        "pre-wrapped lines must fit the pane (38 cells inside borders), got {max_width}"
    );
    assert!(lines.len() > 1, "a long line must break into several rows");
}

#[test]
fn render_uses_the_cache_across_frames() {
    let mut app = App::new("test/model");
    app.timeline.push(TimelineItem::User {
        text: "hello".into(),
    });

    let backend = ratatui::backend::TestBackend::new(80, 20);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal
        .draw(|f| thunder_tui::ui::render(f, &mut app))
        .unwrap();
    assert_eq!(app.transcript.len(), app.timeline.len());

    terminal
        .draw(|f| thunder_tui::ui::render(f, &mut app))
        .unwrap();
    assert_eq!(app.transcript.len(), app.timeline.len());
}
