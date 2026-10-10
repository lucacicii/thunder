use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info};

mod ask_user;
mod attachments;
#[cfg(feature = "testing-mock")]
mod mock;
mod protocol;
mod service;
mod ui;

use protocol::{DaemonRequest, DaemonResponse};
use service::DaemonService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // CRITICAL: Log to stderr so stdout is 100% reserved for NDJSON IPC communication with Electron
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("info,thunder_daemon=debug,thunder_agent_root=debug,thunder_agent_loop=debug,thunder_agent_providers=debug")
        });

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    info!("⚡ Starting Thunder Agent Daemon (STDIO Sidecar mode)...");

    let args: Vec<String> = std::env::args().collect();
    let workspace = args
        .iter()
        .position(|a| a == "--workspace" || a == "-w")
        .and_then(|pos| args.get(pos + 1))
        .map(PathBuf::from);

    // `--config-dir <path>` names thunder's user data root for this process. It
    // is applied to the environment rather than threaded through as a value, so
    // every layer below — the conversation store, the plugin host, and the Node
    // bridge child spawned on the first model call — resolves one root. An
    // explicit flag beats an inherited `THUNDER_CONFIG_DIR`.
    if let Some(pos) = args.iter().position(|a| a == "--config-dir") {
        match args.get(pos + 1) {
            Some(dir) => std::env::set_var(
                thunder_agent_loop::core::paths::THUNDER_CONFIG_DIR_ENV,
                dir,
            ),
            None => {
                eprintln!("--config-dir requires a path");
                std::process::exit(2);
            }
        }
    }

    let service = Arc::new(DaemonService::new(workspace).await?);

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    info!("Thunder Daemon ready. Listening for JSON requests on STDIN...");

    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<DaemonRequest>(trimmed) {
            Ok(req) => {
                service.handle_request(req).await;
            }
            Err(e) => {
                error!(error = %e, raw = %trimmed, "Malformed JSON request received");
                eprintln!("Invalid JSON command: {e}");

                // Attempt to salvage request id so the caller Promise/channel deterministically rejects
                let maybe_id = serde_json::from_str::<serde_json::Value>(trimmed)
                    .ok()
                    .and_then(|v| {
                        v.get("id")
                            .and_then(|id_val| id_val.as_str().map(String::from))
                    });

                let res = DaemonResponse::Response {
                    id: maybe_id,
                    success: false,
                    data: None,
                    error: Some(format!("Invalid JSON command: {e}")),
                };
                service.send_response(res).await;
            }
        }
    }

    info!("STDIN closed (EOF). Thunder Daemon shutting down cleanly.");
    service.shutdown().await;
    Ok(())
}
