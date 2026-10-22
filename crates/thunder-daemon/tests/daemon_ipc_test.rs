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
            question_id,
            questions,
            ..
        } = frame
        {
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

#[tokio::test]
async fn daemon_workspace_locator_api_and_auto_routing() {
    let ws = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let store = Arc::new(FsConversationStore::new(store_dir.path()).await.unwrap());

    let (service, mut rx) = DaemonService::with_store(Some(ws.path().to_path_buf()), store)
        .await
        .unwrap();
    let service = Arc::new(service);

    // 1. BindWorkspace
    let target_dir = ws.path().join("sub_anfeng");
    std::fs::create_dir_all(&target_dir).unwrap();

    let bind_req: DaemonRequest = serde_json::from_value(json!({
        "method": "bind_workspace",
        "id": "b-1",
        "path": target_dir.to_string_lossy(),
        "name": "sub_anfeng",
        "aliases": ["安丰子系统"],
        "keywords": ["运单"]
    }))
    .unwrap();
    service.handle_request(bind_req).await;

    let bind_frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match bind_frame {
        DaemonResponse::Response {
            id,
            success,
            data: Some(d),
            ..
        } => {
            assert_eq!(id.as_deref(), Some("b-1"));
            assert!(success);
            assert_eq!(d["name"], "sub_anfeng");
        }
        other => panic!("unexpected bind response: {other:?}"),
    }

    // 2. ListWorkspaces
    let list_req: DaemonRequest = serde_json::from_value(json!({
        "method": "list_workspaces",
        "id": "l-1"
    }))
    .unwrap();
    service.handle_request(list_req).await;
    let list_frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match list_frame {
        DaemonResponse::Response {
            id,
            success,
            data: Some(d),
            ..
        } => {
            assert_eq!(id.as_deref(), Some("l-1"));
            assert!(success);
            assert!(d.as_array().unwrap().len() >= 1);
        }
        other => panic!("unexpected list response: {other:?}"),
    }

    // 3. LocateWorkspace
    let locate_req: DaemonRequest = serde_json::from_value(json!({
        "method": "locate_workspace",
        "id": "loc-1",
        "prompt": "修改安丰子系统的运单报表"
    }))
    .unwrap();
    service.handle_request(locate_req).await;
    let loc_frame: DaemonResponse = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    match loc_frame {
        DaemonResponse::Response {
            id,
            success,
            data: Some(d),
            ..
        } => {
            assert_eq!(id.as_deref(), Some("loc-1"));
            assert!(success);
            assert_eq!(d["type"], "direct_match");
            assert_eq!(
                std::path::PathBuf::from(d["target_path"].as_str().unwrap())
                    .canonicalize()
                    .unwrap(),
                target_dir.canonicalize().unwrap()
            );
        }
        other => panic!("unexpected locate response: {other:?}"),
    }
}
