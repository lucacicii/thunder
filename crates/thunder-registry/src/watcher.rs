use crate::registry::ToolRegistry;
use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_protocol::{EventSink, ThunderEvent};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Default file watcher debounce window: 200ms (§4.1).
pub const DEFAULT_DEBOUNCE_MS: u64 = 200;

/// Handle to the running `.thunder/tools/` hot-reload watcher actor.
pub struct RegistryWatcherHandle {
    _shutdown_tx: mpsc::Sender<()>,
}

/// Spawn the background hot-reload watcher actor monitoring `tools_dir` with 200ms debounce (§4.1).
pub async fn start_hot_reload_watcher(
    registry: ToolRegistry,
    tools_dir: PathBuf,
    event_sink: Option<Arc<dyn EventSink>>,
) -> Result<RegistryWatcherHandle, std::io::Error> {
    tokio::fs::create_dir_all(&tools_dir).await?;
    let tools_dir = tools_dir.canonicalize().unwrap_or(tools_dir);
    let _ = registry.reload_scripts_dir(&tools_dir).await;

    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);

    let mut debouncer = new_debouncer(
        Duration::from_millis(DEFAULT_DEBOUNCE_MS),
        move |res: notify_debouncer_mini::DebounceEventResult| {
            if res.is_ok() {
                let _ = notify_tx.send(());
            }
        },
    )
    .map_err(std::io::Error::other)?;

    debouncer
        .watcher()
        .watch(&tools_dir, RecursiveMode::NonRecursive)
        .map_err(std::io::Error::other)?;

    tokio::spawn(async move {
        let _keep_debouncer_alive = debouncer;
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    break;
                }
                 maybe_evt = notify_rx.recv() => {
                    if maybe_evt.is_none() {
                        break;
                    }
                    // Drain any coalesced notifications
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
                        Err(e) => {
                            warn!(error = %e, "Failed to reload scripts directory");
                        }
                    }
                }
            }
        }
    });

    Ok(RegistryWatcherHandle {
        _shutdown_tx: shutdown_tx,
    })
}
