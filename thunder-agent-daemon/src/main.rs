use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing::{error, info};

mod mock;
mod protocol;
mod service;

use protocol::DaemonRequest;
use service::DaemonService;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // CRITICAL: Log to stderr so stdout is 100% reserved for NDJSON IPC communication with Electron
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("⚡ Starting Thunder Agent Daemon (STDIO Sidecar mode)...");

    let args: Vec<String> = std::env::args().collect();
    let workspace = args
        .iter()
        .position(|a| a == "--workspace" || a == "-w")
        .and_then(|pos| args.get(pos + 1))
        .map(PathBuf::from);

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
            }
        }
    }

    info!("STDIN closed (EOF). Thunder Daemon shutting down cleanly.");
    service.shutdown().await;
    Ok(())
}
