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
