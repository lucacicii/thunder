//! The App's half of the pane report: what the terminal is told, and when.
//!
//! `pane_status` (the module) covers the protocol's framing and its alphabet;
//! this covers the wiring — that a fresh TUI names itself, that the two waits
//! only this process can see make it into the report, and that a run's phases do
//! too, whether they came from a plugin host (`/auto`) or were folded in by hand
//! (`/single`, which builds no `ThunderRoot`).
//!
//! The assertions go through the same bytes a panel sees, so a report that
//! encodes to something no consumer can parse fails here rather than silently
//! doing nothing in a pane.

use std::time::Duration;
use thunder_agent_loop::prelude::*;
use thunder_tui::ask_user::{AskOption, AskQuestion, IncomingQuestion, PendingQuestion};
use thunder_tui::prelude::App;

/// Reconcile a frame's worth of status and return what the runner would write.
fn report(app: &mut App) -> Option<String> {
    app.reconcile_pane_status();
    app.pane_status.take_pending()
}

/// The body of a sequence, with the two things that decide whether a consumer
/// honours it *at all* checked rather than assumed.
fn body(sequence: &str) -> String {
    let body = sequence
        .strip_prefix("\x1b]7501;")
        .and_then(|rest| rest.strip_suffix("\x1b\\"))
        .unwrap_or_else(|| panic!("not an OSC 7501 sequence: {sequence:?}"));
    assert!(body.contains("app=thunder"), "{body:?}");
    assert!(
        !body.contains("id="),
        "an `id` makes this a child record, which owns no pane: {body:?}"
    );
    body.to_string()
}

fn question() -> PendingQuestion {
    let (responder, _rx) = tokio::sync::oneshot::channel();
    PendingQuestion::from_incoming(IncomingQuestion {
        question_id: "q1".to_string(),
        questions: vec![AskQuestion {
            question: "Proceed?".to_string(),
            header: Some("Approval".to_string()),
            multi_select: false,
            options: vec![AskOption {
                label: "Yes".to_string(),
                description: None,
            }],
        }],
        responder,
    })
    .expect("a non-empty question set")
}

#[test]
fn a_fresh_tui_names_itself_then_says_nothing_more() {
    let mut app = App::new("gpt-4o");
    assert_eq!(
        body(&report(&mut app).expect("the first frame names the pane")),
        "app=thunder:state=idle"
    );
    assert!(
        report(&mut app).is_none(),
        "a report that says nothing new must not be written again"
    );
}

#[test]
fn a_run_walks_the_pane_from_working_through_its_tool_to_done() {
    let mut app = App::new("gpt-4o");
    let session = app.conversation.id.clone();
    // The auto path gets this from the plugin host's `on_init`; `/single` calls
    // it directly. Either way the run's clock restarts before its first event.
    app.status_plugin.begin_run(&session);
    let _ = report(&mut app);

    let send = |app: &mut App, event: AgentEvent| {
        app.status_plugin.record_session_event(&session, &event);
    };

    send(
        &mut app,
        AgentEvent::TurnStart {
            turn: 1,
            timestamp: 0,
            estimated_prompt_tokens: 0,
        },
    );
    assert_eq!(body(&report(&mut app).unwrap()), "app=thunder:state=working");

    send(
        &mut app,
        AgentEvent::ToolExecStart {
            turn: 1,
            tool_call_id: "c1".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
        },
    );
    assert_eq!(
        body(&report(&mut app).unwrap()),
        "app=thunder:state=working:kind=bash"
    );

    // The tool name is free text; a name the protocol cannot carry costs the
    // `kind` pair and nothing else.
    send(
        &mut app,
        AgentEvent::ToolExecResult {
            turn: 1,
            tool_call_id: "c1".to_string(),
            name: "bash".to_string(),
            result: ToolExecutionResult::success("ok".to_string(), Duration::ZERO),
        },
    );
    send(
        &mut app,
        AgentEvent::ToolExecStart {
            turn: 1,
            tool_call_id: "c2".to_string(),
            name: "mcp__gh__create issue".to_string(),
            arguments: serde_json::json!({}),
        },
    );
    assert_eq!(
        body(&report(&mut app).unwrap()),
        "app=thunder:state=working",
        "an unspellable tool name must not take the report down with it"
    );

    send(
        &mut app,
        AgentEvent::LoopComplete {
            finish_reason: FinishReason::Done,
            final_content: None,
            stats: AgentStats::default(),
        },
    );
    assert_eq!(body(&report(&mut app).unwrap()), "app=thunder:state=done");
}

#[test]
fn a_modal_on_screen_is_a_wait_and_releasing_it_restores_the_run() {
    let mut app = App::new("gpt-4o");
    let session = app.conversation.id.clone();
    app.status_plugin.begin_run(&session);
    app.status_plugin.record_session_event(
        &session,
        &AgentEvent::ToolExecStart {
            turn: 1,
            tool_call_id: "c1".to_string(),
            name: "bash".to_string(),
            arguments: serde_json::json!({}),
        },
    );
    let _ = report(&mut app);

    // Five call sites can open a modal and five can close it; the report is
    // reconciled from the state they leave behind, so none of them has to
    // remember to tell the terminal.
    app.pending_question = Some(question());
    assert_eq!(body(&report(&mut app).unwrap()), "app=thunder:state=blocked");

    app.pending_question = None;
    assert_eq!(
        body(&report(&mut app).unwrap()),
        "app=thunder:state=working:kind=bash",
        "answering a dialog returns to the tool the run is actually on"
    );
}

#[test]
fn a_parked_run_is_blocked_and_resuming_reports_it_again() {
    let mut app = App::new("gpt-4o");
    let session = app.conversation.id.clone();
    app.status_plugin.begin_run(&session);
    app.pause_gate = Some(PauseGate::new_shared());
    let _ = report(&mut app);

    app.pause_gate.as_ref().unwrap().pause();
    assert_eq!(body(&report(&mut app).unwrap()), "app=thunder:state=blocked");

    app.pause_gate.as_ref().unwrap().resume();
    assert_eq!(body(&report(&mut app).unwrap()), "app=thunder:state=idle");
}

#[test]
fn a_tui_that_is_leaving_puts_the_pane_back_to_rest() {
    let mut app = App::new("gpt-4o");
    let session = app.conversation.id.clone();
    app.status_plugin.begin_run(&session);
    app.status_plugin.record_session_event(
        &session,
        &AgentEvent::TurnStart {
            turn: 1,
            timestamp: 0,
            estimated_prompt_tokens: 0,
        },
    );
    let _ = report(&mut app);

    app.pane_status.leaving();
    assert_eq!(
        body(&app.pane_status.take_pending().expect("a last report")).as_str(),
        "app=thunder:state=idle"
    );
}
