//! `thunder-runtime` — the generic agent core behind a stdio JSON protocol.
//!
//! It is deliberately smaller than the daemon next to it. There is no session
//! store, no plugin host, no workspace policy, no provider registry: the host
//! says which model to use and *runs the tools itself*. That is the whole point —
//! a prototype editor, a form bot and a data pipeline can all drive the same
//! loop without inheriting any of the others' product decisions.
//!
//! stdout is reserved for protocol frames; logs go to stderr.
//!
//! `--config-dir <path>` names thunder's user data root for this process (the
//! same knob as `THUNDER_CONFIG_DIR`; see `thunder_agent_loop::core::paths`), so
//! a host can keep this binary's scratchpad and conversation state out of the
//! end user's `~/.thunder`.

mod protocol;
mod remote;
mod session;

use protocol::RuntimeRequest;
use session::Runtime;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    info!("thunder-runtime starting");

    // `--config-dir <path>` names thunder's user data root for this process. It
    // is applied to the environment rather than threaded through as a value, so
    // every layer below — the loop's scratchpad among them — resolves one root.
    // An explicit flag beats an inherited `THUNDER_CONFIG_DIR`.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--config-dir") {
        match args.get(pos + 1) {
            Some(dir) => {
                std::env::set_var(thunder_agent_loop::core::paths::THUNDER_CONFIG_DIR_ENV, dir)
            }
            None => {
                eprintln!("--config-dir requires a path");
                std::process::exit(2);
            }
        }
    }

    let runtime = Arc::new(Runtime::new());
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<RuntimeRequest>(trimmed) {
            Ok(req) => runtime.handle(req).await,
            Err(err) => {
                error!(error = %err, "malformed request");
                let id = serde_json::from_str::<serde_json::Value>(trimmed)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string));
                if let Some(id) = id {
                    runtime
                        .reply(Some(id), false, Some(format!("invalid request: {err}")))
                        .await;
                } else {
                    runtime.emit_error(format!("invalid request: {err}"));
                }
            }
        }
    }

    info!("stdin closed; shutting down");
    runtime.shutdown().await;
    Ok(())
}
