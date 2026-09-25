use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::types::tool::ToolDefinition;
use thunder_pi_bridge::model::{BridgeModel, API_OPENAI_COMPLETIONS};
use thunder_pi_bridge::{PiAiBridge, PiAiClient};
use tokio_util::sync::CancellationToken;

fn fake_pi_ai_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fake_pi_ai")
}

async fn node_available() -> bool {
    tokio::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn launch_test_bridge() -> Option<Arc<PiAiBridge>> {
    if !node_available().await {
        eprintln!("skipping: node not available");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("thunder-bridge-test-{}", std::process::id()));
    let bridge = PiAiBridge::launch_with_timeout(
        dir,
        Some(fake_pi_ai_dir()),
        Duration::from_secs(20),
    )
    .await
    .expect("bridge should launch against the fake pi-ai");
    Some(bridge)
}

fn echo_model() -> BridgeModel {
    let mut model = BridgeModel::new("cc-switch", "echo-model", API_OPENAI_COMPLETIONS);
    model.base_url = "https://api.example.com/v1".into();
    model.api_key = Some("sk-test".into());
    model.reasoning = true;
    model
}

fn echo_request() -> ChatRequestOptions {
    ChatRequestOptions {
        messages: vec![
            ChatMessage::system("You are the test system prompt"),
            ChatMessage::user("hello bridge"),
            ChatMessage::assistant(None, Some(vec![ToolCall::new_function(
                "call_9",
                "bash",
                r#"{"command":"ls"}"#,
            )])),
            ChatMessage::tool("call_9", "file_a\nfile_b", Some("bash".to_string())),
        ],
        tools: vec![ToolDefinition::new_function(
            "bash_tool",
            "Run a shell command",
            serde_json::json!({"type":"object","properties":{}}),
        )],
        model: None,
        temperature: Some(0.2),
        top_p: None,
        max_tokens: Some(512),
        thinking_level: Some("high".to_string()),
    }
}

#[tokio::test]
async fn bridge_maps_stream_end_to_end() {
    let Some(bridge) = launch_test_bridge().await else { return };

    let client = PiAiClient::new(bridge, echo_model(), 30_000);
    let cancel = CancellationToken::new();
    let mut rx = client.stream_chat(echo_request(), cancel).await.expect("stream should start");

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut completed = None;
    while let Some(chunk) = rx.recv().await {
        match chunk {
            Ok(LLMStreamChunk::Token(delta)) => text.push_str(&delta),
            Ok(LLMStreamChunk::ReasoningToken(delta)) => reasoning.push_str(&delta),
            Ok(LLMStreamChunk::Completed {
                content,
                tool_calls,
                finish_reason,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                reasoning_tokens,
            }) => {
                completed = Some((
                    content,
                    tool_calls,
                    finish_reason,
                    prompt_tokens,
                    completion_tokens,
                    cached_tokens,
                    reasoning_tokens,
                ));
                break;
            }
            Err(err) => panic!("unexpected stream error: {err}"),
            _ => {}
        }
    }

    assert_eq!(text, "Hello world");
    assert_eq!(reasoning, "pondering");

    let (content, tool_calls, finish_reason, prompt, completion, cached, reasoning_tok) =
        completed.expect("stream must complete");
    assert_eq!(content.as_deref(), Some("Hello world"));
    assert_eq!(finish_reason, "tool_calls");
    // prompt_tokens is the full prompt: uncached input (11) + cache read (4) + cache write (1)
    assert_eq!(prompt, Some(16));
    assert_eq!(completion, Some(22));
    assert_eq!(cached, Some(4));
    assert_eq!(reasoning_tok, Some(6));

    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "bash");
    assert_eq!(tool_calls[0].function.arguments, r#"{"command":"ls"}"#);
}

#[tokio::test]
async fn bridge_maps_error_events() {
    let Some(bridge) = launch_test_bridge().await else { return };

    let mut model = echo_model();
    model.id = "error-model".into();
    let client = PiAiClient::new(bridge, model, 30_000);

    let cancel = CancellationToken::new();
    let mut rx = client.stream_chat(echo_request(), cancel).await.expect("stream should start");

    let mut saw_error = false;
    while let Some(chunk) = rx.recv().await {
        if let Err(err) = chunk {
            assert_eq!(err, "boom");
            saw_error = true;
            break;
        }
    }
    assert!(saw_error, "expected an error event");
}

#[tokio::test]
async fn bridge_cancellation_aborts_stream() {
    let Some(bridge) = launch_test_bridge().await else { return };

    let mut model = echo_model();
    model.id = "slow-model".into();
    let client = PiAiClient::new(bridge, model, 30_000);

    let cancel = CancellationToken::new();
    let mut rx = client
        .stream_chat(echo_request(), cancel.clone())
        .await
        .expect("stream should start");

    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();

    // Either the watchdog's immediate error or the sidecar's aborted event.
    let started = std::time::Instant::now();
    let mut got_err = false;
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("cancellation path must terminate promptly")
    {
        if chunk.is_err() {
            got_err = true;
            break;
        }
    }
    assert!(got_err, "cancelled stream must surface an error");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn bridge_bootstraps_runner_files_into_bridge_dir() {
    if !node_available().await {
        eprintln!("skipping: node not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "thunder-bridge-bootstrap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    assert!(!dir.join("bridge.mjs").exists());
    let bridge = PiAiBridge::launch_with_timeout(
        dir.clone(),
        Some(fake_pi_ai_dir()),
        Duration::from_secs(20),
    )
    .await
    .expect("bootstrap should copy runner files and start");
    assert!(dir.join("bridge.mjs").exists());
    assert!(dir.join("package.json").exists());

    // list_models must answer (fake has no providers/all → empty catalog, not an error)
    let models = bridge.list_models().await.expect("list_models should respond");
    assert!(models.is_empty());
}
