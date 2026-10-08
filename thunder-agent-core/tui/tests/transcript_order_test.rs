//! One chronological transcript: thinking, answers and tool calls land in the
//! order they happened, one assistant block per turn, and the engine's
//! authoritative list at the end of a run does not reorder any of it.

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent, TurnStats};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::types::tool::ToolExecutionResult;
use thunder_tui::prelude::*;
use thunder_tui::ui::theme::Theme;
use tokio::sync::mpsc;

const THINKING: &str = "weighing the options";
const FIRST_ANSWER: &str = "Let me read the config.";
const SECOND_ANSWER: &str = "The config is fine.";
const TOOL_OUTPUT: &str = "first line\nsecond line\nthird line";

fn observed(event: AgentEvent) -> ObservedEvent {
    ObservedEvent {
        agent_id: "agent_1".to_string(),
        event,
        timestamp_ms: 0,
    }
}

fn turn_stats(turn: usize, tool_calls: usize) -> TurnStats {
    TurnStats {
        turn,
        prompt_tokens: Some(10),
        completion_tokens: Some(5),
        cached_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        duration_ms: 12,
        tool_calls_count: tool_calls,
        tokens_per_second: None,
    }
}

fn tool_result() -> ToolExecutionResult {
    ToolExecutionResult {
        output: TOOL_OUTPUT.to_string(),
        is_error: false,
        truncated: false,
        original_bytes: TOOL_OUTPUT.len(),
        duration_ms: 1_200,
        telemetry: None,
    }
}

/// A transcript with one tool-calling turn followed by a plain answer.
fn app_with_two_turns() -> App {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.conversation
        .add_user_message("check the config".to_string());

    app.handle_agent_event(observed(AgentEvent::TurnStart {
        turn: 1,
        timestamp: 1,
    }));
    app.handle_agent_event(observed(AgentEvent::ReasoningDelta {
        turn: 1,
        delta: THINKING.to_string(),
    }));
    app.handle_agent_event(observed(AgentEvent::TokenDelta {
        turn: 1,
        delta: FIRST_ANSWER.to_string(),
    }));
    app.handle_agent_event(observed(AgentEvent::TurnEnd {
        turn: 1,
        finish_reason: "tool_calls".to_string(),
        stats: turn_stats(1, 1),
    }));
    app.handle_agent_event(observed(AgentEvent::ToolCallReady {
        turn: 1,
        tool_call: ToolCall::new_function("call_1", "read_file", "{\"path\":\"Cargo.toml\"}"),
    }));
    app.handle_agent_event(observed(AgentEvent::ToolExecStart {
        turn: 1,
        tool_call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({ "path": "Cargo.toml" }),
    }));
    app.handle_agent_event(observed(AgentEvent::ToolExecResult {
        turn: 1,
        tool_call_id: "call_1".to_string(),
        name: "read_file".to_string(),
        result: tool_result(),
    }));
    app.handle_agent_event(observed(AgentEvent::TurnStart {
        turn: 2,
        timestamp: 2,
    }));
    app.handle_agent_event(observed(AgentEvent::TokenDelta {
        turn: 2,
        delta: SECOND_ANSWER.to_string(),
    }));
    app.handle_agent_event(observed(AgentEvent::TurnEnd {
        turn: 2,
        finish_reason: "stop".to_string(),
        stats: turn_stats(2, 0),
    }));
    app
}

fn rows(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| thunder_tui::ui::draw(f, app, &Theme::default()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
        .collect()
}

fn line_of(rows: &[String], needle: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} should be on screen: {rows:#?}"))
}

#[test]
fn one_turn_is_one_assistant_message_that_keeps_its_tool_calls() {
    let app = app_with_two_turns();

    let assistants: Vec<&ChatMessage> = app
        .conversation
        .messages
        .iter()
        .filter(|message| matches!(message, ChatMessage::Assistant { .. }))
        .collect();
    assert_eq!(assistants.len(), 2, "one assistant message per turn");

    match assistants[0] {
        ChatMessage::Assistant {
            content,
            tool_calls,
            ..
        } => {
            assert_eq!(content.as_deref(), Some(FIRST_ANSWER));
            let calls = tool_calls.as_ref().expect("the turn's call rides along");
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call_1");
        }
        _ => unreachable!(),
    }

    assert!(matches!(
        app.conversation.messages.last(),
        Some(ChatMessage::Assistant { content: Some(text), .. }) if text == SECOND_ANSWER
    ));
}

#[test]
fn the_transcript_is_painted_in_the_order_it_happened() {
    let mut app = app_with_two_turns();
    let rows = rows(&mut app, 100, 30);

    let thinking = line_of(&rows, "Thinking");
    let first = line_of(&rows, FIRST_ANSWER);
    let tool = line_of(&rows, "⚙ read_file");
    let second = line_of(&rows, SECOND_ANSWER);

    assert!(thinking < first, "thinking precedes the answer: {rows:#?}");
    assert!(
        first < tool,
        "the answer precedes the call it made: {rows:#?}"
    );
    assert!(
        tool < second,
        "the next turn's answer comes after the tool: {rows:#?}"
    );
}

#[test]
fn details_are_collapsed_until_ctrl_o() {
    let mut app = app_with_two_turns();

    let collapsed = rows(&mut app, 100, 30);
    assert!(
        collapsed
            .iter()
            .any(|row| row.contains("Thinking · 1 line")),
        "thinking is summarised: {collapsed:#?}"
    );
    assert!(
        collapsed
            .iter()
            .any(|row| row.contains("✔ 1.2s") && row.contains("3 lines")),
        "the tool line carries status and duration: {collapsed:#?}"
    );
    assert!(
        !collapsed.iter().any(|row| row.contains("args:")),
        "collapsed hides the arguments: {collapsed:#?}"
    );
    assert!(
        collapsed.iter().any(|row| row.contains("second line"))
            && !collapsed.iter().any(|row| row.contains("third line")),
        "collapsed previews two rows of output: {collapsed:#?}"
    );

    app.details_expanded = true;
    let expanded = rows(&mut app, 100, 30);
    assert!(
        expanded.iter().any(|row| row.contains("args:")),
        "expanded shows the arguments: {expanded:#?}"
    );
    assert!(
        expanded.iter().any(|row| row.contains("third line")),
        "expanded shows the whole output: {expanded:#?}"
    );
    assert!(
        expanded.iter().any(|row| row.contains(THINKING)),
        "expanded shows the full thinking text: {expanded:#?}"
    );
}

#[test]
fn thinking_survives_the_authoritative_replacement() {
    let mut app = app_with_two_turns();

    // The engine hands back the same transcript minus reasoning, which the host
    // re-attaches by matching the answer it preceded.
    let authoritative = vec![
        ChatMessage::system("system"),
        ChatMessage::user("check the config"),
        ChatMessage::assistant(
            Some(FIRST_ANSWER.to_string()),
            Some(vec![ToolCall::new_function(
                "call_1",
                "read_file",
                "{\"path\":\"Cargo.toml\"}",
            )]),
        ),
        ChatMessage::tool("call_1", TOOL_OUTPUT, Some("read_file".to_string())),
        ChatMessage::assistant(Some(SECOND_ANSWER.to_string()), None),
    ];
    app.handle_agent_finished(
        "agent_1".to_string(),
        true,
        Some(SECOND_ANSWER.to_string()),
        Some(authoritative),
        None,
        None,
        Some("Done".to_string()),
    );

    let rows = rows(&mut app, 100, 30);
    assert!(
        rows.iter().any(|row| row.contains("Thinking · 1 line")),
        "thinking stays attached after the transcript is replaced: {rows:#?}"
    );
    assert!(
        !app.thinking.is_empty(),
        "the side table keeps the turn's thinking"
    );
}

#[test]
fn expanded_tool_output_is_capped() {
    let mut app = App::new("gpt-4o");
    app.conversation.messages.clear();
    app.conversation.add_user_message("run it".to_string());
    app.conversation.add_assistant_message(
        Some("Running now.".to_string()),
        Some(vec![ToolCall::new_function("call_1", "bash", "{}")]),
    );
    let long: String = (0..250)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.conversation
        .add_tool_message("call_1", long, Some("bash".to_string()));
    app.details_expanded = true;

    let rows = rows(&mut app, 100, 80);
    assert!(
        rows.iter().any(|row| row.contains("50 more lines")),
        "the overflow is reported instead of painted: {rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("line 249")),
        "the tail is clipped: {rows:#?}"
    );
}

#[test]
fn the_details_command_drives_the_same_switch() {
    let mut app = app_with_two_turns();
    let (tx, _rx) = mpsc::unbounded_channel();
    assert!(!app.details_expanded());

    app.execute_slash_command("/details on", tx);

    assert!(app.details_expanded(), "/details on expands");
    let rows = rows(&mut app, 100, 30);
    assert!(
        rows.iter().any(|row| row.contains("args:")),
        "the expanded view is what is painted: {rows:#?}"
    );
}
