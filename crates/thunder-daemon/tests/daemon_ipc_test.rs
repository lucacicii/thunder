use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use thunder_core::{FsConversationStore, LlmTurnResponse, ScriptedMockDriver, ToolCallRequest};
use thunder_daemon::{DaemonRequest, DaemonResponse, DaemonService, PROTOCOL_VERSION};

#[tokio::test]
async fn daemon_handles_ping_mode_run_task_and_conversation_persistence() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Executing echo via bash",
            vec![ToolCallRequest {
                id: "call-1".into(),
                name: "core__bash".into(),
                arguments: json!({ "command": "echo daemon_task_ok" }),
            }],
        ),
        LlmTurnResponse::simple("Daemon task completed successfully!", vec![]),
    ]));

    let drv = Arc::clone(&mock_driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    // 1. Ping
    let ping_req: DaemonRequest = serde_json::from_value(json!({
        "method": "ping",
        "id": "p-1"
    }))
    .unwrap();
    service.handle_request(ping_req).await;
    let ping_frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match ping_frame {
        DaemonResponse::Response {
            id,
            success,
            data: Some(d),
            ..
        } => {
            assert_eq!(id.as_deref(), Some("p-1"));
            assert!(success);
            assert_eq!(d["status"], "pong");
            assert_eq!(d["protocol_version"], PROTOCOL_VERSION);
        }
        other => panic!("unexpected ping response: {other:?}"),
    }

    // 2. SetMode -> emits both Response and ModeChanged push frame
    let mode_req: DaemonRequest = serde_json::from_value(json!({
        "method": "set_mode",
        "id": "m-1",
        "session_id": "sess-daemon-1",
        "mode": "plan"
    }))
    .unwrap();
    service.handle_request(mode_req).await;
    let mode_resp: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    assert!(matches!(
        mode_resp,
        DaemonResponse::Response { success: true, .. }
    ));
    let mode_push: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    assert!(matches!(
        mode_push,
        DaemonResponse::ModeChanged { ref mode, .. } if mode == "plan"
    ));

    // 3. RunTask -> streams ObservedEvent frames and finishes with TaskCompleted
    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-1",
        "task_id": "task-100",
        "session_id": "sess-daemon-1",
        "prompt": "Run echo daemon_task_ok",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let mut saw_observed = false;
    let mut final_text = None;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::ObservedEvent { task_id, .. } => {
                assert_eq!(task_id, "task-100");
                saw_observed = true;
            }
            DaemonResponse::TaskCompleted {
                task_id,
                session_id,
                final_content,
                ..
            } => {
                assert_eq!(task_id, "task-100");
                assert_eq!(session_id, "sess-daemon-1");
                final_text = final_content;
                break;
            }
            DaemonResponse::TaskFailed { error, .. } => {
                panic!("task failed unexpectedly: {error}");
            }
            _ => {}
        }
    }
    assert!(saw_observed, "must emit streaming ObservedEvent frames");
    assert_eq!(
        final_text.as_deref(),
        Some("Daemon task completed successfully!")
    );

    // 4. ListConversations & GetConversation
    let list_req: DaemonRequest = serde_json::from_value(json!({
        "method": "list_conversations",
        "id": "lc-1"
    }))
    .unwrap();
    service.handle_request(list_req).await;
    let list_resp: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    assert_eq!(list_resp["data"]["conversations"][0]["id"], "sess-daemon-1");

    let title_req: DaemonRequest = serde_json::from_value(json!({
        "method": "set_conversation_title",
        "id": "st-1",
        "session_id": "sess-daemon-1",
        "title": "Custom Daemon Title"
    }))
    .unwrap();
    service.handle_request(title_req).await;
    let title_resp: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    assert_eq!(title_resp["data"]["ok"], true);
    assert_eq!(title_resp["data"]["title"], "Custom Daemon Title");
}

#[tokio::test]
async fn daemon_handles_user_question_and_answer_cycle() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let mock_driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "Asking user for selection",
            vec![ToolCallRequest {
                id: "call-ask-1".into(),
                name: "core__ask_user_question".into(),
                arguments: json!({
                    "questions": [{
                        "question": "Choose architecture",
                        "options": [{ "label": "Microkernel" }, { "label": "Monolith" }]
                    }]
                }),
            }],
        ),
        LlmTurnResponse::simple("Configured with Microkernel as chosen.", vec![]),
    ]));

    let drv = Arc::clone(&mock_driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-ask",
        "task_id": "task-ask-1",
        "session_id": "sess-ask-1",
        "prompt": "Setup architecture",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    // Read until UserQuestion frame arrives
    let mut captured_qid = None;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        if let DaemonResponse::UserQuestion {
            task_id,
            session_id,
            question_id,
            questions,
            ..
        } = frame
        {
            // The host answers by question id, but finds the run to dock the bubble on
            // by the task id it requested — an invented id leaves it with nothing to
            // show and a run parked until the request timeout.
            assert_eq!(task_id, "task-ask-1", "the frame must name the requesting run");
            assert_eq!(session_id.as_deref(), Some("sess-ask-1"));
            assert_eq!(questions[0].question, "Choose architecture");
            captured_qid = Some(question_id);
            break;
        }
    }

    let qid = captured_qid.expect("must receive UserQuestion frame");

    // Answer the question via answer_question
    let answer_req: DaemonRequest = serde_json::from_value(json!({
        "method": "answer_question",
        "id": "ans-1",
        "question_id": qid,
        "answers": { "Choose architecture": "Microkernel" }
    }))
    .unwrap();
    service.handle_request(answer_req).await;

    // Read until TaskCompleted
    let mut completed = false;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        if let DaemonResponse::TaskCompleted { final_content, .. } = frame {
            assert_eq!(
                final_content.as_deref(),
                Some("Configured with Microkernel as chosen.")
            );
            completed = true;
            break;
        }
    }
    assert!(completed, "task must complete after question is answered");
}

/// `attachments` used to be swallowed by `RunTask`'s `..`: the app sent an image,
/// got `{"started": true}`, and the model received plain text. This is the
/// acceptance test for the fix — the image has to survive the whole way to the
/// stored conversation, where `get_conversation` hands it back to the app.
#[tokio::test]
async fn run_task_attachments_reach_the_persisted_user_message() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "it is blue",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-img",
        "task_id": "task-img",
        "session_id": "sess-img",
        "prompt": "what colour is this?",
        "workspace_dir": ws.path().to_string_lossy(),
        "attachments": [{
            "mimeType": "image/png",
            "name": "shot.png",
            "data": "aGVsbG8="
        }]
    }))
    .unwrap();
    service.handle_request(run_req).await;

    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    // The model itself has to see the image, not just the store.
    let recorded = driver.recorded_messages();
    let seen_by_model = recorded
        .iter()
        .flatten()
        .find(|message| message.image_count() == 1)
        .expect("the driver must be handed a user message carrying the image");
    let image = seen_by_model
        .image_parts()
        .next()
        .and_then(|part| part.as_image())
        .expect("the image part must survive the multimodal message builder");
    assert_eq!(image, ("image/png", "aGVsbG8="));
    assert_eq!(
        seen_by_model.content_str(),
        Some("what colour is this?"),
        "the prompt text must still lead the turn the model sees"
    );

    let get_req: DaemonRequest = serde_json::from_value(json!({
        "method": "get_conversation",
        "id": "g-img",
        "session_id": "sess-img"
    }))
    .unwrap();
    service.handle_request(get_req).await;
    let conv: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();

    let user = conv["data"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .expect("the run must persist its user turn");
    let parts = user["parts"]
        .as_array()
        .expect("the user turn must carry parts");

    assert_eq!(parts.len(), 2, "prompt text plus one image: {parts:?}");
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "what colour is this?");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mimeType"], "image/png");
    assert_eq!(parts[1]["data"], "aGVsbG8=");
    assert_eq!(parts[1]["name"], "shot.png");
}

/// The other half of the fix: an attachment that cannot be delivered must fail the
/// request instead of starting a run without it.
#[tokio::test]
async fn run_task_with_an_undeliverable_attachment_is_rejected_before_the_run_starts() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "should never be reached",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-bad",
        "task_id": "task-bad",
        "session_id": "sess-bad",
        "prompt": "what colour is this?",
        "workspace_dir": ws.path().to_string_lossy(),
        "attachments": [{ "mimeType": "image/png", "name": "ghost.png" }]
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match frame {
        DaemonResponse::Response {
            id, success, error, ..
        } => {
            assert_eq!(id.as_deref(), Some("r-bad"));
            assert!(!success, "an undeliverable attachment must not start a run");
            let error = error.expect("the rejection must say why");
            assert!(error.contains("ghost.png"), "{error}");
            assert!(error.contains("neither `data` nor `path`"), "{error}");
        }
        other => panic!("expected a failed response, got {other:?}"),
    }
    assert!(
        rx.try_recv().is_err(),
        "no task frame may follow a rejected run_task"
    );
}

/// Steering carried images through the same hole as `run_task`, and the steer queue
/// was already built to hold them (`image_count` is reported on `steer_accepted`).
#[tokio::test]
async fn steer_task_attachments_are_queued_as_parts() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    // The first turn sleeps inside a tool call so a steer can land while it runs.
    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "running a slow command",
            vec![ToolCallRequest {
                id: "call-slow".into(),
                name: "core__bash".into(),
                arguments: json!({ "command": "sleep 0.4" }),
            }],
        ),
        LlmTurnResponse::simple("done", vec![]),
    ]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-steer",
        "task_id": "task-steer",
        "session_id": "sess-steer",
        "prompt": "start",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let steer_req: DaemonRequest = serde_json::from_value(json!({
        "method": "steer_task",
        "id": "s-1",
        "task_id": "task-steer",
        "message": "also look at this",
        "behavior": "steer",
        "attachments": [{
            "mimeType": "image/png",
            "name": "second.png",
            "data": "d29ybGQ="
        }]
    }))
    .unwrap();
    service.handle_request(steer_req).await;

    let mut steered = false;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::Response { id, success, .. } if id.as_deref() == Some("s-1") => {
                assert!(success, "a valid attachment must be queued");
            }
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
        steered = true;
    }

    assert!(steered, "the run must have emitted frames");
    let recorded = driver.recorded_messages();
    let steered_turn = recorded
        .iter()
        .flatten()
        .find(|message| {
            message.content_str() == Some("also look at this") && message.image_count() == 1
        })
        .expect("the steered message must reach the model with its image");
    assert_eq!(
        steered_turn
            .image_parts()
            .next()
            .and_then(|part| part.as_image()),
        Some(("image/png", "d29ybGQ="))
    );
}

/// The host's `workspace_dir` is authoritative.
///
/// This used to be re-derived from the prompt by `thunder-locator`: a prompt that
/// merely *mentioned* a registered project name — including the absolute path a
/// host prepends to its own prompt — moved the whole run into that directory, with
/// no frame telling anyone. The registry is gone; this test is what keeps it gone.
#[tokio::test]
async fn run_task_runs_in_the_workspace_the_host_named_whatever_the_prompt_says() {
    let named = tempdir().unwrap();
    let other = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "done",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(named.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    // The prompt points somewhere else entirely, in the shape both desktop hosts use.
    let prompt = format!(
        "[Active Workspace: {}]\n\n把 {} 这个目录里的报表改成分页",
        named.path().display(),
        other.path().display()
    );
    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-ws",
        "task_id": "task-ws",
        "session_id": "sess-ws",
        "prompt": prompt,
        "workspace_dir": named.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let get_req: DaemonRequest = serde_json::from_value(json!({
        "method": "get_conversation",
        "id": "g-ws",
        "session_id": "sess-ws"
    }))
    .unwrap();
    service.handle_request(get_req).await;
    let conv: Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();

    let recorded = conv["data"]["workspace"]
        .as_str()
        .expect("the conversation must record the workspace it ran in");
    assert_eq!(
        std::path::PathBuf::from(recorded).canonicalize().unwrap(),
        named.path().canonicalize().unwrap(),
        "the run must not follow the prompt into another directory"
    );
}

/// `headless` used to be swallowed by `RunTask`'s `..`, so a scheduled run could ask a
/// question nobody was there to answer: the model spent a turn, waited out
/// `tool_timeout`, and got an error. A tool the host cannot answer must not be in the
/// catalogue at all.
#[tokio::test]
async fn headless_runs_are_not_offered_a_question_tool_they_cannot_answer() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "done",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-headless",
        "task_id": "task-headless",
        "session_id": "sess-headless",
        "prompt": "summarise yesterday",
        "workspace_dir": ws.path().to_string_lossy(),
        "headless": true
    }))
    .unwrap();
    service.handle_request(run_req).await;

    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let catalogues = driver.recorded_tool_prefixes();
    assert!(!catalogues.is_empty(), "the driver must have been called");
    for catalogue in &catalogues {
        assert!(
            !catalogue.contains("ask_user_question"),
            "a headless run must not be offered the question tool: {catalogue}"
        );
    }
}

#[tokio::test]
async fn a_non_headless_run_still_offers_the_question_tool() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "done",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-interactive",
        "task_id": "task-interactive",
        "session_id": "sess-interactive",
        "prompt": "do the thing",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let catalogue = driver.recorded_tool_prefixes().join("\n");
    assert!(
        catalogue.contains("ask_user_question"),
        "an interactive run must still be able to ask: {catalogue}"
    );
}

/// `extra_workspace_dirs` was declared and ignored, so a second repository in scope was
/// invisible to every search. This drives the real `thunder-std` with no `path` argument
/// and insists the file in the additional directory comes back.
#[tokio::test]
async fn extra_workspace_dirs_put_a_second_directory_in_search_scope() {
    let primary = tempdir().unwrap();
    let extra = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());
    std::fs::write(
        extra.path().join("shared-note.txt"),
        "hidden_in_the_extra_root",
    )
    .unwrap();

    let driver = Arc::new(ScriptedMockDriver::new(vec![
        LlmTurnResponse::simple(
            "searching both roots",
            vec![ToolCallRequest {
                id: "call-grep".into(),
                name: "core__grep".into(),
                arguments: json!({ "pattern": "hidden_in_the_extra_root" }),
            }],
        ),
        LlmTurnResponse::simple("found it", vec![]),
    ]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(primary.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-extra",
        "task_id": "task-extra",
        "session_id": "sess-extra",
        "prompt": "find the shared note",
        "workspace_dir": primary.path().to_string_lossy(),
        "extra_workspace_dirs": [extra.path().to_string_lossy()]
    }))
    .unwrap();
    service.handle_request(run_req).await;

    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { .. } => break,
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let tool_outputs: String = driver
        .recorded_messages()
        .iter()
        .flatten()
        .filter_map(|message| match message {
            thunder_protocol::ChatMessage::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        tool_outputs.contains("shared-note.txt"),
        "the additional root must be searched: {tool_outputs}"
    );
}

#[tokio::test]
async fn an_extra_workspace_dir_that_does_not_exist_rejects_the_run() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "should never be reached",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-missing",
        "task_id": "task-missing",
        "session_id": "sess-missing",
        "prompt": "do the thing",
        "workspace_dir": ws.path().to_string_lossy(),
        "extra_workspace_dirs": ["/definitely/not/a/real/directory/here"]
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match frame {
        DaemonResponse::Response {
            id, success, error, ..
        } => {
            assert_eq!(id.as_deref(), Some("r-missing"));
            assert!(!success);
            let error = error.expect("the rejection must say why");
            assert!(error.contains("extra_workspace_dirs"), "{error}");
        }
        other => panic!("expected a failed response, got {other:?}"),
    }
}

/// `active_plugins` must report what answered, not what was intended.
///
/// The daemon discovers `thunder-std` as a sibling binary. A host that ships the
/// daemon without it gets a run with **zero tools**; the frame used to say
/// `["thunder-std", "kv-cache-shield"]` anyway, which is precisely the wrong answer
/// for the one case a host needs it: a broken sidecar.
#[tokio::test]
async fn a_run_without_thunder_std_does_not_claim_to_have_it() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "done",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(
        service
            .with_driver_factory(Arc::new(move |_| {
                Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
            }))
            // A path that cannot start: same effect as a missing sibling binary.
            .with_std_binary(std::path::PathBuf::from("/nonexistent/thunder-std")),
    );

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-nostd",
        "task_id": "task-nostd",
        "session_id": "sess-nostd",
        "prompt": "do the thing",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let mut plugins: Option<Vec<String>> = None;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { active_plugins, .. } => {
                plugins = Some(active_plugins);
                break;
            }
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let plugins = plugins.expect("the run must complete");
    assert!(
        !plugins.iter().any(|id| id == "thunder-std"),
        "a run with no tool pack must not report one: {plugins:?}"
    );
    assert!(
        plugins.iter().any(|id| id == "kv-cache-shield"),
        "the kernel facility is always mounted: {plugins:?}"
    );
}

/// The other half: with the real binary present, it is reported.
#[tokio::test]
async fn a_run_that_has_thunder_std_reports_it() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let driver = Arc::new(ScriptedMockDriver::new(vec![LlmTurnResponse::simple(
        "done",
        vec![],
    )]));
    let drv = Arc::clone(&driver);
    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service.with_driver_factory(Arc::new(move |_| {
        Some(Arc::clone(&drv) as Arc<dyn thunder_core::LlmDriver>)
    })));

    let run_req: DaemonRequest = serde_json::from_value(json!({
        "method": "run_task",
        "id": "r-std",
        "task_id": "task-std",
        "session_id": "sess-std",
        "prompt": "do the thing",
        "workspace_dir": ws.path().to_string_lossy()
    }))
    .unwrap();
    service.handle_request(run_req).await;

    let mut plugins: Option<Vec<String>> = None;
    while let Some(line) = rx.recv().await {
        let frame: DaemonResponse = serde_json::from_str(&line).unwrap();
        match frame {
            DaemonResponse::TaskCompleted { active_plugins, .. } => {
                plugins = Some(active_plugins);
                break;
            }
            DaemonResponse::TaskFailed { error, .. } => panic!("task failed: {error}"),
            _ => {}
        }
    }

    let plugins = plugins.expect("the run must complete");
    assert!(
        plugins.iter().any(|id| id == "thunder-std"),
        "the sibling binary is present in this build, so it must be reported: {plugins:?}"
    );
}
