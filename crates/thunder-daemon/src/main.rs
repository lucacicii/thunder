use std::path::PathBuf;
use std::sync::Arc;
use thunder_daemon::{DaemonRequest, DaemonResponse, DaemonService};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{error, info};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let workspace = args
        .iter()
        .position(|a| a == "--workspace" || a == "-w")
        .and_then(|pos| args.get(pos + 1))
        .map(PathBuf::from);

    if let Some(pos) = args.iter().position(|a| a == "--config-dir") {
        match args.get(pos + 1) {
            Some(dir) => {
                std::env::set_var(thunder_protocol::THUNDER_CONFIG_DIR_ENV, dir);
            }
            None => {
                eprintln!("--config-dir requires a path");
                std::process::exit(2);
            }
        }
    }

    let (service, mut output_rx) = DaemonService::new(workspace).await?;
    let service = Arc::new(service);

    // Spawn dedicated stdout writer task for NDJSON responses/events.
    tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = output_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });

    info!("Thunder Daemon ready on STDIN/STDOUT (protocol v3)");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

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
                error!(error = %e, "Malformed NDJSON request");
                let maybe_id = serde_json::from_str::<serde_json::Value>(trimmed)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str().map(String::from)));
                service
                    .send_response(DaemonResponse::Response {
                        id: maybe_id,
                        success: false,
                        data: None,
                        error: Some(format!("Invalid JSON command: {e}")),
                    })
                    .await;
            }
        }
    }

    service.shutdown().await;
    Ok(())
}
