//! Regression: a real (non-mock) stdio MCP server must survive `spawn` and
//! answer requests.
//!
//! The mock transport cannot see this class of bug: it never spawns a process.
//! `StdioTransport::spawn` sets `kill_on_drop(true)` on the `Command`, so the
//! `Child` handle has to outlive the call — when it was a local that went out of
//! scope at the end of `spawn`, the server was SIGKILLed the instant it started
//! and every handshake failed (instantly with `NotConnected`, or after the 10 s
//! initialize timeout).

use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_mcp::client::McpClient;
use thunder_agent_mcp::config::McpServerConfig;
use thunder_agent_mcp::protocol::JsonRpcRequest;
use thunder_agent_mcp::transport::stdio::StdioTransport;
use thunder_agent_mcp::transport::McpTransport;

/// A minimal MCP server: one JSON-RPC frame per line in, one out.
const FAKE_SERVER: &str = r#"#!/bin/sh
# $1 is a unique marker so the test can find this exact process.
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"fake-mcp","version":"1.0.0"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

fn write_fake_server(dir: &Path) -> PathBuf {
    write_server(dir, "fake-mcp-server.sh", FAKE_SERVER)
}

/// A server that reads every request and answers none of them, ever.
const SILENT_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  :
done
"#;

fn write_silent_server(dir: &Path) -> PathBuf {
    write_server(dir, "silent-mcp-server.sh", SILENT_SERVER)
}

fn write_server(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write server script");
    let mut perms = std::fs::metadata(&path)
        .expect("stat server script")
        .permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&path, perms).expect("chmod server script");

    path
}

/// A marker unique to one test run, so `pgrep` can find that test's server only.
///
/// Both tests share this process, so the pid alone is not enough; the counter
/// makes the marker unique even when the clock resolution is coarse.
fn unique_marker() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "thunder-mcp-probe-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Live processes whose command line carries this marker, as `pid cmdline`.
fn processes_with_marker(marker: &str) -> Vec<String> {
    let out = std::process::Command::new("pgrep")
        .args(["-fl", marker])
        .output()
        .expect("run pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

fn config_for(script: &Path, marker: &str) -> McpServerConfig {
    McpServerConfig::new("sh")
        .with_arg(script.to_string_lossy().to_string())
        .with_arg(marker.to_string())
}

#[tokio::test]
async fn a_real_stdio_server_survives_spawn_and_serves_requests() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_fake_server(dir.path());
    let marker = unique_marker();
    let cfg = config_for(&script, &marker);

    let client = McpClient::connect_stdio("fake", &cfg)
        .await
        .expect("initialize handshake against a live server");

    assert!(
        client.is_alive(),
        "server must still be running after the handshake"
    );
    let alive = processes_with_marker(&marker);
    assert_eq!(
        alive.len(),
        1,
        "exactly one server process must be alive while connected, got {alive:?}"
    );

    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool("echo", Some(json!({ "text": "hi" })))
        .await
        .expect("tools/call");
    assert_eq!(result.plain_text(), "pong");

    // A second request after the first keeps working: the transport is reusable,
    // not a one-shot that happens to win a race.
    client.ping().await.expect("ping");

    client.close().await.expect("close");
    assert!(!client.is_alive(), "close() must mark the transport dead");
    // `Child::kill` is awaited by `close`, so reaping is done by the time it returns.
    let after_close = processes_with_marker(&marker);
    assert!(
        after_close.is_empty(),
        "close() must terminate the server process, still alive: {after_close:?}"
    );
}

/// The consumer-visible half: a configured workspace server must yield bridged
/// tools. `McpPlugin::on_init` swallows connect errors, so a broken transport
/// shows up here as "zero tools" rather than as a failure.
#[tokio::test]
async fn a_configured_server_yields_bridged_tools() {
    use thunder_agent_mcp::config::McpConfig;
    use thunder_agent_mcp::manager::McpManager;

    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_fake_server(dir.path());
    let marker = unique_marker();
    let mut config = McpConfig::new();
    config
        .mcp_servers
        .insert("fake".to_string(), config_for(&script, &marker));

    let manager = McpManager::from_config(config).await.expect("manager");
    assert_eq!(manager.list_servers().await, vec!["fake".to_string()]);

    let tools = manager.discover_all_tools().await;
    assert_eq!(tools.len(), 1, "the fake server exposes exactly one tool");
    assert_eq!(tools[0].definition().function.name, "mcp_fake_echo");

    manager.close_all().await;
    let after_close = processes_with_marker(&marker);
    assert!(
        after_close.is_empty(),
        "close_all must terminate the server process, still alive: {after_close:?}"
    );
}

/// A server that never answers must not park its caller forever, and must not
/// leave its in-flight entry behind either.
#[tokio::test]
async fn a_silent_server_times_out_and_leaves_no_pending_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_silent_server(dir.path());
    let marker = unique_marker();
    let cfg = config_for(&script, &marker).with_timeout_ms(300);

    let transport = StdioTransport::spawn("silent", &cfg).await.expect("spawn");

    let started = std::time::Instant::now();
    let err = transport
        .send_request(JsonRpcRequest::new(json!(1), "ping", None))
        .await
        .expect_err("a silent server must not answer");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "the call must be bounded by the configured timeout, took {elapsed:?}"
    );
    assert!(
        err.to_string().contains("did not answer"),
        "the error must name the cause, got: {err}"
    );
    assert_eq!(
        transport.pending_len(),
        0,
        "a timed-out request must not linger in the pending table"
    );
    assert!(
        transport.is_alive(),
        "a slow server is not a dead one: the transport stays usable"
    );

    transport.close().await.expect("close");
}

/// `timeout_ms = 0` is the documented opt-out, so the bound must not apply.
#[tokio::test]
async fn an_explicit_zero_timeout_opts_out_of_the_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = write_silent_server(dir.path());
    let marker = unique_marker();
    let cfg = config_for(&script, &marker).with_timeout_ms(0);

    let transport = Arc::new(StdioTransport::spawn("silent", &cfg).await.expect("spawn"));
    assert_eq!(transport.pending_len(), 0);

    let sender = Arc::clone(&transport);
    let parked = tokio::spawn(async move {
        sender
            .send_request(JsonRpcRequest::new(json!(1), "ping", None))
            .await
    });

    // Well past where a 120 s default would fire, but the opt-out means it waits.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        transport.pending_len(),
        1,
        "with timeout_ms = 0 the request is expected to still be in flight"
    );
    assert!(
        !parked.is_finished(),
        "the call must not have been cut short"
    );

    // Closing releases it: nothing is coming back, so the waiter resolves rather
    // than staying parked for the life of the process.
    transport.close().await.expect("close");
    let outcome = tokio::time::timeout(Duration::from_secs(5), parked)
        .await
        .expect("the parked call must resolve once the transport closes")
        .expect("task must not panic");
    assert!(outcome.is_err(), "a closed transport answers nothing");
    assert_eq!(
        transport.pending_len(),
        0,
        "close() must release every parked entry"
    );
}
