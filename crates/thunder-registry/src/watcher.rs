use crate::registry::ToolRegistry;
use notify::Watcher;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_protocol::{EventSink, ThunderEvent};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How often `.thunder/tools/` is polled for changes.
///
/// This used to be a 200 ms "debounce" on top of the platform's native watcher
/// (`notify`'s FSEvents backend on macOS). Measured on macOS, FSEvents delivered the
/// first event 0.9 s, 5.4 s and 8.4 s after a write across three consecutive rounds —
/// unbounded and unpredictable, which is why the hot-reload tests were flaky and why
/// hot reload looked broken to users. A poller gives a bounded, predictable latency
/// (≤ one interval) for the handful of small files this directory ever holds.
///
/// See `docs/known-issues.md` KI-1.
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 500;

/// Deprecated alias kept so downstream code and config keep compiling.
#[deprecated(note = "the watcher polls now; use DEFAULT_POLL_INTERVAL_MS")]
pub const DEFAULT_DEBOUNCE_MS: u64 = DEFAULT_POLL_INTERVAL_MS;

/// Handle to the running `.thunder/tools/` hot-reload watcher actor.
pub struct RegistryWatcherHandle {
    _shutdown_tx: mpsc::Sender<()>,
}

/// Spawn the background hot-reload watcher actor monitoring `tools_dir`.
pub async fn start_hot_reload_watcher(
    registry: ToolRegistry,
    tools_dir: PathBuf,
    event_sink: Option<Arc<dyn EventSink>>,
) -> Result<RegistryWatcherHandle, std::io::Error> {
    start_hot_reload_watcher_with_interval(
        registry,
        tools_dir,
        event_sink,
        Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
    )
    .await
}

/// Same as [`start_hot_reload_watcher`] with an explicit poll interval.
pub async fn start_hot_reload_watcher_with_interval(
    registry: ToolRegistry,
    tools_dir: PathBuf,
    event_sink: Option<Arc<dyn EventSink>>,
    poll_interval: Duration,
) -> Result<RegistryWatcherHandle, std::io::Error> {
    tokio::fs::create_dir_all(&tools_dir).await?;
    let tools_dir = tools_dir.canonicalize().unwrap_or(tools_dir);
    let _ = registry.reload_scripts_dir(&tools_dir).await;

    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    // `compare_contents` hashes file bodies instead of trusting mtime+size, so an
    // atomic-replace save (write temp file, rename over) is never missed. The cost is
    // bounded: this directory holds a handful of small script files.
    let mut watcher = notify::poll::PollWatcher::new(
        move |res: Result<notify::Event, notify::Error>| match res {
            Ok(event) => {
                debug!(kind = ?event.kind, "tools dir changed");
                let _ = notify_tx.send(());
            }
            Err(err) => warn!(error = %err, "tools dir poll failed"),
        },
        notify::Config::default()
            .with_poll_interval(poll_interval)
            .with_compare_contents(true),
    )
    .map_err(std::io::Error::other)?;

    watcher
        .watch(&tools_dir, notify::RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;

    tokio::spawn(async move {
        // Keep the watcher alive for the lifetime of the actor.
        let _keep_watcher_alive = watcher;
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => break,
                maybe_evt = notify_rx.recv() => {
                    if maybe_evt.is_none() {
                        break;
                    }
                    // Coalesce anything queued behind this poll.
                    while notify_rx.try_recv().is_ok() {}

                    let before = registry.current();
                    match registry.reload_scripts_dir(&tools_dir).await {
                        Ok(after) => {
                            let added: Vec<String> = after
                                .descriptors
                                .keys()
                                .filter(|k| !before.descriptors.contains_key(*k))
                                .cloned()
                                .collect();
                            info!(
                                version = after.version,
                                total_tools = after.descriptors.len(),
                                added_count = added.len(),
                                "CatalogSnapshot hot-reloaded via ArcSwap"
                            );
                            if let Some(ref sink) = event_sink {
                                sink.on_event(ThunderEvent::RegistryUpdated {
                                    version: after.version,
                                    added_tools: added,
                                    shadowed_tools: after.shadowed_uris.clone(),
                                });
                            }
                        }
                        Err(e) => warn!(error = %e, "Failed to reload scripts directory"),
                    }
                }
            }
        }
    });

    Ok(RegistryWatcherHandle {
        _shutdown_tx: shutdown_tx,
    })
}
