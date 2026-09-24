use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[tokio::test]
async fn test_daemon_ping_and_mock_run() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut reader = BufReader::new(stdout).lines();

    // 1. Test Ping
    let ping_req = serde_json::json!({
        "method": "ping",
        "id": "req-ping-1"
    });
    stdin
        .write_all(format!("{}\n", ping_req).as_bytes())
        .await?;
    stdin.flush().await?;

    let line = reader.next_line().await?.expect("Expected ping response");
    let resp: serde_json::Value = serde_json::from_str(&line)?;
    assert_eq!(resp["type"], "response");
    assert_eq!(resp["id"], "req-ping-1");
    assert_eq!(resp["success"], true);
    assert_eq!(resp["data"]["pong"], true);

    // 2. Test RunTask with Mock
    let run_req = serde_json::json!({
        "method": "run_task",
        "id": "req-run-1",
        "task_id": "task-test-1",
        "prompt": "Hello Thunder!",
        "use_mock": true
    });
    stdin.write_all(format!("{}\n", run_req).as_bytes()).await?;
    stdin.flush().await?;

    // Wait for task ack
    let ack_line = reader.next_line().await?.expect("Expected task ack");
    let ack: serde_json::Value = serde_json::from_str(&ack_line)?;
    assert_eq!(ack["type"], "response");
    assert_eq!(ack["id"], "req-run-1");
    assert_eq!(ack["success"], true);

    // Receive streaming events until completed
    let mut got_token_or_event = false;
    let mut completed = false;

    while let Ok(Some(evt_line)) = reader.next_line().await {
        let evt: serde_json::Value = serde_json::from_str(&evt_line)?;
        if evt["type"] == "observed_event" {
            got_token_or_event = true;
        } else if evt["type"] == "task_completed" {
            assert_eq!(evt["task_id"], "task-test-1");
            completed = true;
            break;
        }
    }

    assert!(completed, "Task should reach completed state");
    assert!(got_token_or_event, "Should have received observed events");

    // 3. Test GetTrace
    let session_id = ack["data"]["session_id"].as_str().unwrap();
    let trace_req = serde_json::json!({
        "method": "get_trace",
        "id": "req-trace-1",
        "session_id": session_id,
        "task_id": "task-test-1"
    });
    stdin.write_all(format!("{}\n", trace_req).as_bytes()).await?;
    stdin.flush().await?;

    let trace_line = reader.next_line().await?.expect("Expected trace response");
    let trace_resp: serde_json::Value = serde_json::from_str(&trace_line)?;
    assert_eq!(trace_resp["type"], "response");
    assert_eq!(trace_resp["id"], "req-trace-1");
    assert_eq!(trace_resp["success"], true);
    assert_eq!(trace_resp["data"]["task_id"], "task-test-1");
    assert!(trace_resp["data"]["events"].is_array());

    // 4. Test ListTraces
    let list_traces_req = serde_json::json!({
        "method": "list_traces",
        "id": "req-list-traces-1",
        "session_id": session_id,
    });
    stdin.write_all(format!("{}\n", list_traces_req).as_bytes()).await?;
    stdin.flush().await?;

    let list_traces_line = reader.next_line().await?.expect("Expected list traces response");
    let list_traces_resp: serde_json::Value = serde_json::from_str(&list_traces_line)?;
    assert_eq!(list_traces_resp["type"], "response");
    assert_eq!(list_traces_resp["id"], "req-list-traces-1");
    assert_eq!(list_traces_resp["success"], true);
    assert!(!list_traces_resp["data"]["traces"].as_array().unwrap().is_empty());

    // Close stdin to trigger clean shutdown
    drop(stdin);
    let status = child.wait().await?;
    assert!(status.success());

    Ok(())
}

#[tokio::test]
async fn test_daemon_list_models_and_cancel() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut reader = BufReader::new(stdout).lines();

    // 1. List models
    let list_req = serde_json::json!({
        "method": "list_models",
        "id": "req-models-1"
    });
    stdin
        .write_all(format!("{}\n", list_req).as_bytes())
        .await?;
    stdin.flush().await?;

    let line = reader.next_line().await?.expect("Expected models response");
    let resp: serde_json::Value = serde_json::from_str(&line)?;
    assert_eq!(resp["type"], "response");
    assert_eq!(resp["id"], "req-models-1");
    assert_eq!(resp["success"], true);
    assert!(resp["data"]["models"].is_array());

    // 2. Start a task and cancel it immediately
    let run_req = serde_json::json!({
        "method": "run_task",
        "id": "req-run-cancel",
        "task_id": "task-to-cancel",
        "prompt": "Long prompt that will be cancelled",
        "use_mock": true
    });
    stdin.write_all(format!("{}\n", run_req).as_bytes()).await?;
    stdin.flush().await?;

    let ack_line = reader.next_line().await?.expect("Expected task ack");
    let ack: serde_json::Value = serde_json::from_str(&ack_line)?;
    assert_eq!(ack["id"], "req-run-cancel");

    // Cancel it
    let cancel_req = serde_json::json!({
        "method": "cancel_task",
        "id": "req-cancel-1",
        "task_id": "task-to-cancel"
    });
    stdin.write_all(format!("{}\n", cancel_req).as_bytes()).await?;
    stdin.flush().await?;

    // Drain until we get cancellation response or completion
    let mut got_cancel_ack = false;
    while let Ok(Some(evt_line)) = reader.next_line().await {
        let evt: serde_json::Value = serde_json::from_str(&evt_line)?;
        if evt["id"] == "req-cancel-1" {
            assert_eq!(evt["success"], true);
            got_cancel_ack = true;
            break;
        }
    }
    assert!(got_cancel_ack);

    drop(stdin);
    let status = child.wait().await?;
    assert!(status.success());

    Ok(())
}

#[tokio::test]
async fn test_run_task_extra_workspace_dirs_merge() -> Result<(), Box<dyn std::error::Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_thunder-daemon"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    let stdout = child.stdout.take().expect("Failed to open stdout");
    let mut reader = BufReader::new(stdout).lines();

    let repo_a = std::env::temp_dir().join("thunder_ipc_repo_a");
    let repo_b = std::env::temp_dir().join("thunder_ipc_repo_b");
    std::fs::create_dir_all(&repo_a)?;
    std::fs::create_dir_all(&repo_b)?;

    async fn send(
        stdin: &mut tokio::process::ChildStdin,
        payload: String,
    ) -> std::io::Result<()> {
        stdin.write_all(format!("{}\n", payload).as_bytes()).await?;
        stdin.flush().await
    }

    // First run binds workspace + one extra root.
    let run1 = serde_json::json!({
        "method": "run_task",
        "id": "req-merge-1",
        "task_id": "task-merge-1",
        "prompt": "hello",
        "use_mock": true,
        "session_id": "sess-merge-test",
        "workspace_dir": std::env::temp_dir().join("thunder_ipc_ws").to_string_lossy(),
        "extra_workspace_dirs": [repo_a.to_string_lossy()]
    });
    send(&mut stdin, run1.to_string()).await?;

    let mut shared_roots: Vec<String> = Vec::new();
    // Drain until the ack for req-merge-1 shows shared_roots.
    while let Ok(Some(line)) = reader.next_line().await {
        let evt: serde_json::Value = serde_json::from_str(&line)?;
        if evt["type"] == "response" && evt["id"] == "req-merge-1" {
            assert_eq!(evt["success"], true);
            let roots = evt["data"]["shared_roots"].as_array().expect("shared_roots echoed");
            shared_roots = roots.iter().map(|v| v.as_str().unwrap().to_string()).collect();
            break;
        }
        // Ignore task events; the mock run may complete quickly.
    }
    assert_eq!(shared_roots.len(), 1, "first run binds exactly one extra root: {shared_roots:?}");
    assert!(shared_roots[0].ends_with("thunder_ipc_repo_a"));

    // Drain any events from the mock run finishing.
    while let Ok(Some(line)) = reader.next_line().await {
        let evt: serde_json::Value = serde_json::from_str(&line)?;
        if evt["type"] == "task_completed" {
            break;
        }
    }

    // Second run on the same session: workspace stays bound (merge policy),
    // a new repo joins, re-sending repo_a must not duplicate.
    let run2 = serde_json::json!({
        "method": "run_task",
        "id": "req-merge-2",
        "task_id": "task-merge-2",
        "prompt": "hello again",
        "use_mock": true,
        "session_id": "sess-merge-test",
        "workspace_dir": "/this/ignored/new/workspace",
        "extra_workspace_dirs": [repo_a.to_string_lossy(), repo_b.to_string_lossy()]
    });
    send(&mut stdin, run2.to_string()).await?;

    let mut merged: Vec<String> = Vec::new();
    let mut bound_workspace = String::new();
    while let Ok(Some(line)) = reader.next_line().await {
        let evt: serde_json::Value = serde_json::from_str(&line)?;
        if evt["type"] == "response" && evt["id"] == "req-merge-2" {
            assert_eq!(evt["success"], true);
            let roots = evt["data"]["shared_roots"].as_array().expect("shared_roots echoed");
            merged = roots.iter().map(|v| v.as_str().unwrap().to_string()).collect();
            bound_workspace = evt["data"]["workspace"].as_str().unwrap().to_string();
            break;
        }
    }
    assert_eq!(merged.len(), 2, "deduped union expected: {merged:?}");
    assert!(merged.iter().any(|r| r.ends_with("thunder_ipc_repo_a")));
    assert!(merged.iter().any(|r| r.ends_with("thunder_ipc_repo_b")));
    assert!(
        bound_workspace.ends_with("thunder_ipc_ws"),
        "workspace stays first-bind immutable, got {bound_workspace}"
    );

    drop(stdin);
    let _ = child.wait().await;
    let _ = std::fs::remove_dir_all(&repo_a);
    let _ = std::fs::remove_dir_all(&repo_b);
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("thunder_ipc_ws"));
    Ok(())
}
