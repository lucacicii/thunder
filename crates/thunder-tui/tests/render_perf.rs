//! Measures where a frame's time actually goes.
//!
//! Run with output to see per-stage timings:
//!   cargo test -p thunder-tui --test render_perf -- --nocapture

use ratatui::{backend::TestBackend, Terminal};
use std::time::Instant;
use thunder_tui::{ui, App, TimelineItem};

fn transcript(turns: usize) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    for i in 0..turns {
        items.push(TimelineItem::User {
            text: format!("第 {i} 轮：请检查并重构 src/module_{i}/handler.rs 的实现，注意并发安全与错误处理。"),
        });
        items.push(TimelineItem::Assistant {
            content: format!(
                "## 第 {i} 轮分析\n\n\
                 这里是对 `src/module_{i}/handler.rs` 的分析结果。\n\n\
                 - 第一条要点：存在锁竞争\n\
                 - 第二条要点：错误未封装\n\n\
                 ```rust\n\
                 pub async fn handle_{i}(req: Request) -> Result<Response> {{\n    // 处理第 {i} 轮请求\n    let guard = STATE.lock().await;\n    Ok(Response::ok(guard.len()))\n}}\n\
                 ```\n\n\
                 | 指标 | 数值 | 说明 |\n|---|---|---|\n| 延迟 | {i}ms | p99 |\n| 吞吐 | {} rps | 峰值 |\n",
                i * 10
            ),
            reasoning: String::new(),
            streaming: false,
        });
        items.push(TimelineItem::ToolCallCard {
            call_id: format!("call-{i}"),
            name: "core__read_file".to_string(),
            args_summary: format!("{{\"path\":\"src/module_{i}/handler.rs\"}}"),
            status: thunder_tui::ToolCardStatus::Success {
                output_preview: "…".to_string(),
                duration_ms: 12,
            },
        });
    }
    items
}

#[test]
fn frame_cost_breakdown() {
    let mut app = App::new("test/model");
    app.timeline = transcript(40);
    let items = app.timeline.len();

    // Warm the syntect syntax/theme lazy statics so we time steady state.
    let warm = TestBackend::new(140, 50);
    let mut warm_term = Terminal::new(warm).unwrap();
    warm_term.draw(|f| ui::render(f, &mut app)).unwrap();

    let backend = TestBackend::new(140, 50);
    let mut terminal = Terminal::new(backend).unwrap();

    let frames = 20;
    let start = Instant::now();
    for _ in 0..frames {
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
    }
    let per_frame = start.elapsed() / frames;

    println!(
        "\n[render_perf] items={items} (turns=40) frame={}x{} -> {:.2?}/frame ({:.1} fps max)",
        terminal.size().unwrap().width,
        terminal.size().unwrap().height,
        per_frame,
        1.0 / per_frame.as_secs_f64()
    );

    // Streaming: one assistant item grows every frame; everything else is static.
    let mut streaming = App::new("test/model");
    streaming.timeline = transcript(40);
    streaming.timeline.push(TimelineItem::Assistant {
        content: String::new(),
        reasoning: String::new(),
        streaming: true,
    });

    let start = Instant::now();
    for step in 0..20 {
        if let Some(TimelineItem::Assistant { content, .. }) = streaming.timeline.last_mut() {
            content.push_str("流式输出的一段文字，模拟大模型逐字吐出的内容。\n");
        }
        let _ = step;
        terminal.draw(|f| ui::render(f, &mut streaming)).unwrap();
    }
    let per_frame = start.elapsed() / 20;
    println!(
        "[render_perf] streaming, 1 growing item + 120 static -> {:.2?}/frame ({:.1} fps max)\n",
        per_frame,
        1.0 / per_frame.as_secs_f64()
    );
}
