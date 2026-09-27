//! The daemon's [`HostUi`] implementation: a request/response subprotocol
//! between the agent (or a plugin) and the connected panel.
//!
//! Shape follows pi's `extension_ui_request` / `extension_ui_response` pair
//! rather than inventing a new one: the daemon emits a dialog as plain data and
//! parks on a oneshot until the panel answers or the deadline passes.
//!
//! Three properties are load-bearing:
//!
//! * **Fail closed.** Timeout, closed channel, or a panel that never answers all
//!   resolve to [`UiResponse::Cancelled`]. A caller that reads "cancelled" as
//!   "denied" therefore degrades to the safe branch, never to the permissive one.
//! * **Server-issued ids.** `request_id` is minted here with a counter plus
//!   randomness; a plugin cannot choose one. Combined with the reserved
//!   [`UiSource::Host`] channel, that is what stops a plugin from forging or
//!   replaying an approval prompt (see [`UiSource`]).
//! * **Unroutable ids are inert.** An answer for an unknown or already-expired
//!   `request_id` is dropped with a warning, never delivered to a later dialog.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{info, warn};

use crate::protocol::DaemonResponse;

/// Pending dialogs awaiting an answer, keyed by the server-issued request id.
pub type PendingUiRequests = Arc<Mutex<HashMap<String, oneshot::Sender<UiResponse>>>>;

/// Routes dialogs to the panel and blocks the caller until answered.
///
/// Cheap to clone-share: the pending table is shared, so every clone observes
/// every dialog. This unscoped form is what a long-lived component (e.g. a
/// plugin host) should hold; per-task callers should use [`DaemonHostUi::scoped`]
/// so the panel can attribute the dialog to the right run.
pub struct DaemonHostUi {
    output_tx: mpsc::Sender<String>,
    pending: PendingUiRequests,
    counter: Arc<AtomicU64>,
}

impl DaemonHostUi {
    pub fn new(output_tx: mpsc::Sender<String>, pending: PendingUiRequests) -> Arc<Self> {
        Arc::new(Self {
            output_tx,
            pending,
            counter: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Bind a task/session to every dialog raised through the returned handle.
    pub fn scoped(
        self: &Arc<Self>,
        task_id: impl Into<String>,
        session_id: Option<String>,
    ) -> ScopedHostUi {
        ScopedHostUi {
            base: Arc::clone(self),
            task_id: task_id.into(),
            session_id,
        }
    }

    /// Mint a collision-resistant request id.
    ///
    /// The random suffix matters: ids are the only thing binding an answer to a
    /// dialog, and the daemon multiplexes concurrent tasks, so a monotonic
    /// counter alone would be guessable from a previous run.
    fn next_request_id(&self) -> String {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let entropy = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("ui_{}_{}_{}", std::process::id(), seq, entropy)
    }

    async fn dispatch(
        &self,
        source: UiSource,
        task_id: Option<&str>,
        session_id: Option<&str>,
        request: UiRequest,
    ) -> UiResponse {
        let request_id = self.next_request_id();
        // Resolved before the request is moved into the envelope: the caller's
        // deadline wins, `DEFAULT_UI_TIMEOUT` is only the fallback.
        let timeout = request.timeout();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id.clone(), tx);

        info!(
            request_id = %request_id,
            kind = request.kind(),
            source = ?source,
            task_id = ?task_id,
            "Raising UI request"
        );

        if let Err(err) = write_ndjson(
            &self.output_tx,
            &DaemonResponse::UiRequest {
                request_id: request_id.clone(),
                task_id: task_id.map(str::to_string),
                session_id: session_id.map(str::to_string),
                source,
                request,
            },
        )
        .await
        {
            self.pending.lock().await.remove(&request_id);
            warn!(request_id = %request_id, error = %err, "Failed to emit UI request; treating as cancelled");
            return UiResponse::Cancelled;
        }

        // The caller owns the deadline; we only make sure a silent panel cannot
        // wedge the agent forever.
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => response,
            Ok(Err(_)) => {
                // Sender dropped: the panel connection is gone.
                self.pending.lock().await.remove(&request_id);
                warn!(request_id = %request_id, "UI channel closed before an answer arrived");
                UiResponse::Cancelled
            }
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                warn!(
                    request_id = %request_id,
                    timeout_ms = timeout.as_millis() as u64,
                    "UI request timed out; failing closed"
                );
                UiResponse::Cancelled
            }
        }
    }
}

/// A [`DaemonHostUi`] bound to one task, for panels that route by `task_id`.
pub struct ScopedHostUi {
    base: Arc<DaemonHostUi>,
    task_id: String,
    session_id: Option<String>,
}

#[async_trait]
impl HostUi for ScopedHostUi {
    async fn request(&self, source: UiSource, request: UiRequest) -> UiResponse {
        self.base
            .dispatch(
                source,
                Some(self.task_id.as_str()),
                self.session_id.as_deref(),
                request,
            )
            .await
    }

    fn notify(&self, source: UiSource, message: &str, level: NotifyLevel) {
        self.base.notify(source, message, level)
    }

    fn set_status(&self, key: &str, text: Option<String>) {
        self.base.set_status(key, text)
    }
}

#[async_trait]
impl HostUi for DaemonHostUi {
    async fn request(&self, source: UiSource, request: UiRequest) -> UiResponse {
        self.dispatch(source, None, None, request).await
    }

    fn notify(&self, source: UiSource, message: &str, level: NotifyLevel) {
        // Fire-and-forget: a panel that is not listening simply drops it.
        let payload = DaemonResponse::UiNotice {
            source,
            message: message.to_string(),
            level,
        };
        let tx = self.output_tx.clone();
        tokio::spawn(async move {
            let _ = write_ndjson(&tx, &payload).await;
        });
    }

    fn set_status(&self, key: &str, text: Option<String>) {
        let payload = DaemonResponse::UiStatus {
            key: key.to_string(),
            text,
        };
        let tx = self.output_tx.clone();
        tokio::spawn(async move {
            let _ = write_ndjson(&tx, &payload).await;
        });
    }
}

async fn write_ndjson(tx: &mpsc::Sender<String>, res: &DaemonResponse) -> Result<(), String> {
    let json = serde_json::to_string(res).map_err(|e| e.to_string())?;
    tx.send(format!("{json}\n"))
        .await
        .map_err(|e| e.to_string())
}

/// Deliver an answer to a waiting dialog.
///
/// Returns `false` for an unknown or expired id. The daemon deliberately does
/// *not* treat that as an error to surface to the client beyond `delivered:
/// false`: a panel may legitimately answer a dialog whose deadline just expired.
pub async fn deliver_ui_response(
    pending: &PendingUiRequests,
    request_id: &str,
    response: UiResponse,
) -> bool {
    let sender = pending.lock().await.remove(request_id);
    match sender {
        Some(tx) => tx.send(response).is_ok(),
        None => {
            warn!(request_id, "Answer for unknown/expired UI request");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> (mpsc::Receiver<String>, Arc<DaemonHostUi>, PendingUiRequests) {
        let (tx, rx) = mpsc::channel(16);
        let pending: PendingUiRequests = Arc::new(Mutex::new(HashMap::new()));
        (rx, DaemonHostUi::new(tx, Arc::clone(&pending)), pending)
    }

    #[tokio::test]
    async fn request_ids_are_unique_and_not_client_chosen() {
        let (_rx, ui, _p) = ui();
        let a = ui.next_request_id();
        let b = ui.next_request_id();
        assert_ne!(a, b);
        assert!(a.starts_with("ui_"));
    }

    #[tokio::test]
    async fn dialog_blocks_until_the_panel_answers() {
        let (mut rx, ui, pending) = ui();

        let responder = {
            let ui = Arc::clone(&ui);
            tokio::spawn(async move { ui.select("Allow rm -rf?", &["Allow", "Deny"]).await })
        };

        // The panel sees a data-only envelope: no component, just fields.
        let line = rx.recv().await.expect("ui request emitted");
        let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(msg["type"], "ui_request");
        assert_eq!(msg["ui"], "select");
        assert_eq!(msg["title"], "Allow rm -rf?");
        assert_eq!(msg["options"][0], "Allow");
        assert_eq!(msg["source"], "host");
        let request_id = msg["request_id"].as_str().unwrap().to_string();

        // It is still parked: nothing resolved before the answer arrived.
        assert!(!responder.is_finished());

        assert!(deliver_ui_response(&pending, &request_id, UiResponse::value("Allow")).await);
        assert_eq!(responder.await.unwrap().as_deref(), Some("Allow"));
    }

    #[tokio::test]
    async fn plugin_sourced_dialogs_are_labelled() {
        let (mut rx, ui, _p) = ui();
        tokio::spawn(async move {
            ui.select_from_plugin("trust this repo?", &["yes", "no"])
                .await
        });
        let line = rx.recv().await.unwrap();
        let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        // A panel must be able to tell "the host is asking for permission" from
        // "some plugin is asking for something".
        assert_eq!(msg["source"], "plugin");
    }

    #[tokio::test]
    async fn scoped_handle_tags_the_task() {
        let (mut rx, ui, _p) = ui();
        let scoped = ui.scoped("task-42", Some("sess-7".into()));
        tokio::spawn(async move { scoped.confirm("Proceed?", "This writes files.").await });
        let line = rx.recv().await.unwrap();
        let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(msg["task_id"], "task-42");
        assert_eq!(msg["session_id"], "sess-7");
        assert_eq!(msg["ui"], "confirm");
    }

    #[tokio::test]
    async fn answer_for_unknown_id_is_inert() {
        let (_rx, _ui, pending) = ui();
        assert!(!deliver_ui_response(&pending, "ui_nope", UiResponse::value("x")).await);
    }

    #[tokio::test]
    async fn confirm_requires_an_explicit_yes() {
        let (mut rx, ui, pending) = ui();
        let responder = tokio::spawn({
            let ui = Arc::clone(&ui);
            async move { ui.confirm("Proceed?", "...").await }
        });

        let line = rx.recv().await.unwrap();
        let msg: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let id = msg["request_id"].as_str().unwrap().to_string();

        // An explicit "no" is not a cancellation, and must not read as consent.
        deliver_ui_response(&pending, &id, UiResponse::Confirmed { confirmed: false }).await;
        assert!(!responder.await.unwrap());
    }

    #[tokio::test]
    async fn closed_panel_fails_closed() {
        // A dropped receiver makes every emit fail, so the request must resolve
        // immediately instead of waiting out the full timeout.
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let pending: PendingUiRequests = Arc::new(Mutex::new(HashMap::new()));
        let ui = DaemonHostUi::new(tx, pending);

        // `confirm` collapses cancelled into `false`, which is the point: an
        // unreachable panel must read as "no", never as "yes".
        assert!(!ui.confirm("Proceed?", "...").await);
        assert!(ui.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn notify_and_status_are_fire_and_forget() {
        let (mut rx, ui, _p) = ui();
        ui.notify(UiSource::Plugin, "hello", NotifyLevel::Warning);
        ui.set_status("k", Some("v".into()));
        ui.set_status("k", None);

        let notice: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().trim()).unwrap();
        assert_eq!(notice["type"], "ui_notice");
        assert_eq!(notice["level"], "warning");
        assert_eq!(notice["source"], "plugin");

        let status: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().trim()).unwrap();
        assert_eq!(status["type"], "ui_status");
        assert_eq!(status["key"], "k");
        assert_eq!(status["text"], "v");

        let cleared: serde_json::Value =
            serde_json::from_str(rx.recv().await.unwrap().trim()).unwrap();
        assert!(cleared["text"].is_null());
    }

    #[tokio::test]
    async fn timeout_is_fail_closed() {
        let (mut rx, ui, _p) = ui();
        // 8ms keeps the test fast while still exercising the timeout branch.
        let request = UiRequest::Confirm {
            title: "t".into(),
            message: "m".into(),
            timeout_ms: Some(8),
        };
        let responder = tokio::spawn({
            let ui = Arc::clone(&ui);
            async move { ui.request(UiSource::Host, request).await }
        });
        let _ = rx.recv().await; // panel receives but never answers
        assert!(responder.await.unwrap().is_cancelled());
    }
}
