use std::path::PathBuf;
use std::sync::Arc;
use thunder_daemon::{DaemonRequest, DaemonResponse, DaemonService, PROTOCOL_VERSION};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{error, info, warn};

/// Per-process runtime marker consumed by `thunderctl status` / `thunderctl reload`.
fn runtime_state_path(pid: u32) -> PathBuf {
    thunder_protocol::thunder_subdir("state").join(format!("daemon-{pid}.json"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Publish pid / binary / version / reload capability. Rewritten by the re-exec'd
/// process, which is exactly how `thunderctl reload` confirms the swap happened.
fn write_runtime_state(pid: u32) {
    let path = runtime_state_path(pid);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let payload = serde_json::json!({
        "pid": pid,
        "exe": exe,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": PROTOCOL_VERSION,
        "hot_reload": true,
        "started_at_ms": now_ms(),
        "config_dir": thunder_protocol::thunder_config_dir().display().to_string(),
    });
    if let Ok(text) = serde_json::to_string_pretty(&payload) {
        if let Err(err) = std::fs::write(&path, text) {
            warn!(error = %err, path = %path.display(), "Failed to write runtime state");
        }
    }
}

fn remove_runtime_state(pid: u32) {
    let _ = std::fs::remove_file(runtime_state_path(pid));
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// `SIGUSR1` → replace the running image with the binary currently on disk.
///
/// The pid, argv and the stdin/stdout pipes the desktop app connected to survive the
/// `exec`, so the panel keeps its sidecar without a restart. Refused while tasks run.
async fn handle_reload_signal(service: &Arc<DaemonService>) {
    let running = service.active_task_count().await;
    if running > 0 {
        warn!(
            running,
            "SIGUSR1 hot reload refused: {running} task(s) still running"
        );
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            error!(error = %err, "SIGUSR1 hot reload: cannot resolve own binary");
            return;
        }
    };
    match std::fs::metadata(&exe) {
        Ok(meta) if meta.is_file() && is_executable(&meta) => {}
        Ok(_) => {
            error!(exe = %exe.display(), "SIGUSR1 hot reload refused: binary is not executable");
            return;
        }
        Err(err) => {
            error!(error = %err, exe = %exe.display(), "SIGUSR1 hot reload refused: binary vanished");
            return;
        }
    }

    // Give the NDJSON writer task a moment to flush any frame that is in flight
    // before the address space is replaced.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;

    let forwarded: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    info!(exe = %exe.display(), "SIGUSR1 hot reload: re-executing in place");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&forwarded).exec();
        error!(error = %err, "SIGUSR1 hot reload failed; staying on the current binary");
    }
    #[cfg(not(unix))]
    {
        let _ = forwarded;
        error!("SIGUSR1 hot reload is only supported on unix targets");
    }
}

/// Resolves when `SIGUSR1` arrives; never resolves when the platform has no signals.
async fn wait_for_reload(signal: &mut Option<tokio::signal::unix::Signal>) -> Option<()> {
    match signal {
        Some(signal) => signal.recv().await,
        None => std::future::pending().await,
    }
}

async fn handle_line(service: &Arc<DaemonService>, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    match serde_json::from_str::<DaemonRequest>(trimmed) {
        Ok(req) => {
            service.handle_request(req).await;
        }
        Err(err) => {
            error!(error = %err, "Malformed NDJSON request");
            let maybe_id = serde_json::from_str::<serde_json::Value>(trimmed)
                .ok()
                .and_then(|v| v.get("id").and_then(|i| i.as_str().map(String::from)));
            service
                .send_response(DaemonResponse::Response {
                    id: maybe_id,
                    success: false,
                    data: None,
                    error: Some(format!("Invalid JSON command: {err}")),
                })
                .await;
        }
    }
}

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

    let mut reload_signal =
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
            Ok(signal) => Some(signal),
            Err(err) => {
                warn!(error = %err, "SIGUSR1 hot reload unavailable");
                None
            }
        };

    let pid = std::process::id();
    write_runtime_state(pid);
    info!("Thunder Daemon ready on STDIN/STDOUT (protocol v{PROTOCOL_VERSION})");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        tokio::select! {
            line = reader.next_line() => match line {
                Ok(Some(line)) => handle_line(&service, &line).await,
                Ok(None) => break,
                Err(err) => {
                    error!(error = %err, "Failed to read NDJSON from stdin");
                    break;
                }
            },
            _ = wait_for_reload(&mut reload_signal) => {
                handle_reload_signal(&service).await;
            }
        }
    }

    service.shutdown().await;
    remove_runtime_state(pid);
    Ok(())
}
