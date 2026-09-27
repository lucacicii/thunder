//! Run-control feature tests: pause/unpause, thinking level, permission tier,
//! extra roots, ask_user modal flow, manual titles, and trace persistence.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Duration;
use tempfile::tempdir;
use thunder_agent_loop::types::config::Permission;
use thunder_agent_loop::types::event::{AgentEvent, AgentStats, ObservedEvent};
use thunder_agent_loop::types::message::ToolCall;
use thunder_conversation::prelude::*;
use thunder_tui::app::App;
use thunder_tui::ask_user::{AskOption, AskQuestion, IncomingQuestion};
use tokio::sync::{mpsc, oneshot};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

fn char_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty())
}

// ── /pause · /unpause ─────────────────────────────────────────────────────

#[tokio::test]
async fn pause_and_unpause_toggle_the_run_gate() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    // Without a run there is nothing to pause.
    app.execute_slash_command("/pause", tx.clone());
    assert!(!app.is_paused());

    // Simulate a running task holding a gate.
    let gate = thunder_agent_loop::prelude::PauseGate::new_shared();
    app.pause_gate = Some(gate.clone());
    app.agent_status = thunder_tui::prelude::AgentStatus::Thinking;

    app.execute_slash_command("/pause", tx.clone());
    assert!(app.is_paused(), "gate must be paused after /pause");
    assert!(gate.is_paused());

    app.execute_slash_command("/unpause", tx.clone());
    assert!(!app.is_paused(), "gate must be released after /unpause");
    assert!(!gate.is_paused());
}

#[tokio::test]
async fn finishing_a_run_clears_the_pause_gate() {
    let mut app = App::new("gpt-4o");
    app.pause_gate = Some(thunder_agent_loop::prelude::PauseGate::new_shared());
    app.trace_meta = None;

    app.handle_agent_finished("agent".to_string(), true, None, None, None, None, None);
    assert!(app.pause_gate.is_none(), "gate is cleared on finish");
}

// ── /think ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn think_command_binds_the_level_to_the_conversation() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    assert_eq!(app.effective_thinking_level(), None);
    assert!(app.execute_slash_command("/think high", tx.clone()));
    assert_eq!(app.thinking_level.as_deref(), Some("high"));
    assert_eq!(app.conversation.thinking_level.as_deref(), Some("high"));
    assert_eq!(app.effective_thinking_level().as_deref(), Some("high"));

    app.execute_slash_command("/think off", tx.clone());
    assert_eq!(app.effective_thinking_level().as_deref(), Some("off"));

    // Invalid level is rejected without clobbering the previous binding.
    app.execute_slash_command("/think bananas", tx.clone());
    assert_eq!(app.effective_thinking_level().as_deref(), Some("off"));
}

// ── /permission ───────────────────────────────────────────────────────────

#[tokio::test]
async fn permission_command_sets_the_capability_tier() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    assert_eq!(app.permission, Permission::Bash, "default tier is Bash");

    assert!(app.execute_slash_command("/permission read", tx.clone()));
    assert_eq!(app.permission, Permission::Read);

    app.execute_slash_command("/perm write", tx.clone());
    assert_eq!(app.permission, Permission::Write);

    app.execute_slash_command("/perm bash", tx.clone());
    assert_eq!(app.permission, Permission::Bash);

    app.execute_slash_command("/perm nonsense", tx.clone());
    assert_eq!(app.permission, Permission::Bash, "invalid tier is ignored");
}

// ── /roots ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn roots_command_manages_extra_workspace_roots() {
    let dir = tempdir().unwrap();
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    let add_cmd = format!("/roots add {}", dir.path().display());
    assert!(app.execute_slash_command(&add_cmd, tx.clone()));
    assert_eq!(app.extra_roots.len(), 1);
    assert_eq!(app.extra_roots[0], dir.path());

    // Non-existent directories are rejected.
    app.execute_slash_command("/roots add /nonexistent/definitely", tx.clone());
    assert_eq!(app.extra_roots.len(), 1);

    // Remove by index.
    app.execute_slash_command("/roots remove 0", tx.clone());
    assert!(app.extra_roots.is_empty());
}

#[tokio::test]
async fn submit_prompt_mirrors_roots_into_the_conversation() {
    let dir = tempdir().unwrap();
    let mut app = App::new("gpt-4o").with_client_factory(noop_factory());
    let (tx, _rx) = mpsc::unbounded_channel();
    app.extra_roots.push(dir.path().to_path_buf());

    app.submit_prompt("hello".to_string(), tx);

    assert_eq!(
        app.conversation.shared_roots,
        vec![dir.path().display().to_string()]
    );
    assert_eq!(
        app.conversation.model.as_deref(),
        Some(app.model.selection_id().as_str())
    );
    assert!(app.pause_gate.is_some(), "a gate exists for the run");
    assert!(app.trace_meta.is_some(), "a trace recording started");
}

fn noop_factory() -> thunder_tui::app::ClientFactory {
    use async_trait::async_trait;
    use std::sync::Arc;
    use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
    use thunder_agent_loop::types::config::AgentConfig;

    struct HangingClient;
    #[async_trait]
    impl LLMClientTrait for HangingClient {
        async fn stream_chat(
            &self,
            _options: ChatRequestOptions,
            cancel: tokio_util::sync::CancellationToken,
        ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                cancel.cancelled().await;
            });
            let _ = tx;
            Ok(rx)
        }
    }
    Arc::new(|_cfg: &AgentConfig| Some(Arc::new(HangingClient) as Arc<dyn LLMClientTrait>))
}

// ── /role (resolution + permission derivation) ─────────────────────────────

#[tokio::test]
async fn resolved_roles_derive_the_permission_tier() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    // Drain the RoleResolved event that spawn_role_resolution emits.
    app.execute_slash_command("/role plan", tx.clone());

    // Directly exercise the handler path the runner dispatches to.
    let role = thunder_agent_root::roles::RoleSpec {
        id: "plan".to_string(),
        name: Some("Planner".to_string()),
        aliases: vec![],
        description: None,
        persona: thunder_agent_root::roles::Persona::Text("Plan first.".to_string()),
        permission: Permission::Read,
        model: None,
        thinking_level: Some("high".to_string()),
        ask_user: true,
        exit_gate: false,
        enabled: true,
        triggers: vec![],
    };
    app.attach_resolved_role(role, Permission::Read);

    assert_eq!(app.active_role.as_ref().unwrap().id, "plan");
    assert_eq!(app.permission, Permission::Read, "tier follows the role");

    // Detach restores the permissive default.
    assert!(app.execute_slash_command("/role off", tx.clone()));
    assert!(app.active_role.is_none());
    assert_eq!(app.permission, Permission::Bash);
}

// ── ask_user modal flow ───────────────────────────────────────────────────

#[tokio::test]
async fn question_modal_answers_options_and_frees_the_agent() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    let (res_tx, res_rx) = oneshot::channel();
    let incoming = IncomingQuestion {
        question_id: "tui:q1".to_string(),
        questions: vec![AskQuestion {
            question: "Which layer?".to_string(),
            header: Some("Scope".to_string()),
            multi_select: true,
            options: vec![
                AskOption {
                    label: "renderer".to_string(),
                    description: Some("UI only".to_string()),
                },
                AskOption {
                    label: "main".to_string(),
                    description: None,
                },
            ],
        }],
        responder: res_tx,
    };
    app.pending_question = thunder_tui::ask_user::PendingQuestion::from_incoming(incoming);

    // Select both options (multi-select): first option, toggle, move, toggle.
    app.handle_key(key(KeyCode::Char(' ')), tx.clone());
    app.handle_key(key(KeyCode::Down), tx.clone());
    app.handle_key(key(KeyCode::Char(' ')), tx.clone());
    app.handle_key(key(KeyCode::Enter), tx.clone());

    assert!(app.pending_question.is_none(), "modal closed after answer");
    let payload = tokio::time::timeout(Duration::from_secs(2), res_rx)
        .await
        .expect("responder resolves")
        .unwrap();
    let map = payload.as_object().expect("answers map");
    assert_eq!(
        map["Which layer?"].as_str(),
        Some("renderer, main"),
        "multi-select joins the toggled labels"
    );
}

#[tokio::test]
async fn question_modal_supports_free_form_input() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    let (res_tx, res_rx) = oneshot::channel();
    let incoming = IncomingQuestion {
        question_id: "tui:q1".to_string(),
        questions: vec![AskQuestion {
            question: "Branch name?".to_string(),
            header: None,
            multi_select: false,
            options: vec![],
        }],
        responder: res_tx,
    };
    app.pending_question = thunder_tui::ask_user::PendingQuestion::from_incoming(incoming);

    for c in "feat/notch".chars() {
        app.handle_key(char_key(c), tx.clone());
    }
    // Enter on non-empty input submits.
    app.handle_key(key(KeyCode::Enter), tx.clone());

    let payload = res_rx.await.unwrap();
    assert_eq!(payload["Branch name?"].as_str(), Some("feat/notch"));
}

#[tokio::test]
async fn question_modal_escape_dismisses() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    let (res_tx, res_rx) = oneshot::channel();
    let incoming = IncomingQuestion {
        question_id: "tui:q1".to_string(),
        questions: vec![AskQuestion {
            question: "q".to_string(),
            header: None,
            multi_select: false,
            options: vec![],
        }],
        responder: res_tx,
    };
    app.pending_question = thunder_tui::ask_user::PendingQuestion::from_incoming(incoming);

    app.handle_key(key(KeyCode::Esc), tx);
    assert!(app.pending_question.is_none());
    assert!(res_rx.await.unwrap().is_null(), "dismissal sends Null");
}

// ── /title ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manual_title_is_set_and_locked() {
    let mut app = App::new("gpt-4o");
    let (tx, _rx) = mpsc::unbounded_channel();

    assert!(app.execute_slash_command("/title Rust workspace audit", tx.clone()));
    assert_eq!(
        app.conversation.title.as_deref(),
        Some("Rust workspace audit")
    );
    assert!(app.conversation.is_title_manual());

    // A manual title blocks plain /title (regeneration); force path still runs.
    assert!(!app.should_autogenerate_title());
    assert!(!app.conversation.is_title_placeholder());
}

// ── traces ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn finished_runs_persist_a_trace_and_stats_merge_additively() {
    let tmp = tempdir().unwrap();
    let store = FsConversationStore::new(tmp.path()).await.unwrap();
    let mut app = App::new("gpt-4o").with_store(store);

    // Drive a run through submit → events → finish.
    let (tx, _rx) = mpsc::unbounded_channel();
    app.submit_prompt("do the thing".to_string(), tx);
    let task_id = app.trace_meta.as_ref().unwrap().task_id.clone();
    assert!(app.trace_events.is_some());

    app.handle_agent_event(ObservedEvent {
        agent_id: "a".to_string(),
        event: AgentEvent::TurnStart {
            turn: 1,
            timestamp: 1,
        },
    });
    app.handle_agent_event(ObservedEvent {
        agent_id: "a".to_string(),
        event: AgentEvent::TokenDelta {
            turn: 1,
            delta: "ignored-micro-delta".to_string(),
        },
    });
    app.handle_agent_event(ObservedEvent {
        agent_id: "a".to_string(),
        event: AgentEvent::ToolCallReady {
            turn: 1,
            tool_call: ToolCall::new_function("c1", "grep", "{}"),
        },
    });

    let stats = AgentStats {
        total_turns: 2,
        total_prompt_tokens: 100,
        total_completion_tokens: 50,
        total_cached_tokens: 0,
        total_reasoning_tokens: 0,
        total_duration_ms: 1234,
        total_tool_executions: 3,
        total_tool_time_ms: 100,
        avg_tokens_per_second: None,
    };
    let turns_before = app.conversation.stats.turn_count;
    app.handle_agent_finished(
        "a".to_string(),
        true,
        Some("done".to_string()),
        None,
        None,
        Some(stats),
        Some("Done".to_string()),
    );

    // Additive stats bookkeeping.
    assert_eq!(app.conversation.stats.turn_count, turns_before + 2);

    // Trace persisted (spawned task needs a moment).
    let trace_path = tmp
        .path()
        .join(&app.conversation.id)
        .join("traces")
        .join(format!("{task_id}.json"));
    for _ in 0..50 {
        if trace_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let raw = std::fs::read_to_string(&trace_path).expect("trace file written");
    let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(val["prompt"], "do the thing");
    assert_eq!(val["finish_reason"], "Done");
    assert!(
        !raw.contains("ignored-micro-delta"),
        "micro-deltas filtered"
    );

    // The trace view renders it back.
    let out = thunder_tui::app::render_trace_markdown(&val);
    assert!(out.contains("Task Trace"));
    assert!(out.contains("turn 1 started"));
}
