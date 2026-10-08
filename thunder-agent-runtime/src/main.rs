//! `thunder-runtime` — the generic agent core behind a stdio JSON protocol.
//!
//! It is deliberately smaller than the daemon next to it. There is no session
//! store, no plugin host, no workspace policy, no provider registry: the host
//! says which model to use and *runs the tools itself*. That is the whole point —
//! a prototype editor, a form bot and a data pipeline can all drive the same
//! loop without inheriting any of the others' product decisions.
//!
//! stdout is reserved for protocol frames; logs go to stderr.

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
                let _ = id;
                runtime.emit_error(format!("invalid request: {err}"));
            }
        }
    }

    info!("stdin closed; shutting down");
    runtime.shutdown().await;
    Ok(())
}
