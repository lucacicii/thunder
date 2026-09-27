//! Host-owned user-interaction capability.
//!
//! The agent loop and every plugin may *ask* the user a question, but only the
//! host knows how (or whether) that question reaches a screen. This trait is
//! that boundary, modelled on the loop's existing "product-neutral primitive"
//! rule (see [`crate::core::pause`]): the loop knows how to wait for an answer,
//! nothing about who provides it.
//!
//! Deliberate design constraints:
//!
//! * **Data, not components.** A request carries a title and a list of strings —
//!   never a renderable object. That is what lets one trait serve a full-screen
//!   TUI, an Electron panel, and a headless test harness.
//! * **Fail closed.** Every blocking call resolves to [`UiResponse::Cancelled`]
//!   when the host cannot answer (no UI, timeout, channel closed). A caller that
//!   treats "cancelled" as "deny" therefore degrades safely.
//! * **`source` is not decoration.** [`UiSource::Host`] is the reserved channel
//!   for host-initiated interactions such as permission approval. A client MUST
//!   render it distinctly from [`UiSource::Plugin`], otherwise a plugin can
//!   impersonate the approval prompt (see the threat note on the enum).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Fallback wait for a blocking UI request when the caller sets no timeout.
pub const DEFAULT_UI_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    Info,
    Warning,
    Error,
}

/// Origin of a UI interaction.
///
/// # Threat model
///
/// `Plugin` requests are attacker-reachable in any deployment that loads
/// third-party plugins: a plugin may emit a `select` titled "Allow `rm -rf`?"
/// and hope the user approves. Clients MUST therefore render `Host` requests with
/// reserved chrome that a plugin cannot produce, and MUST NOT treat a
/// `Plugin`-sourced answer as an authorisation decision. Only the host decides
/// permissions; a plugin can at most *display* something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiSource {
    Host,
    Plugin,
}

/// A blocking interaction request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ui", rename_all = "snake_case")]
pub enum UiRequest {
    /// Pick one of `options`. Responds with [`UiResponse::Value`].
    Select {
        title: String,
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Yes/no question. Responds with [`UiResponse::Confirmed`].
    Confirm {
        title: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Single-line free text. Responds with [`UiResponse::Value`].
    Input {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Multi-line free text. Responds with [`UiResponse::Value`].
    Editor {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
}

impl UiRequest {
    /// The caller-supplied timeout, or [`DEFAULT_UI_TIMEOUT`].
    ///
    /// Implementations must apply it themselves and resolve to
    /// [`UiResponse::Cancelled`] on expiry — the loop cannot enforce it.
    pub fn timeout(&self) -> Duration {
        let ms = match self {
            UiRequest::Select { timeout_ms, .. }
            | UiRequest::Confirm { timeout_ms, .. }
            | UiRequest::Input { timeout_ms, .. }
            | UiRequest::Editor { timeout_ms, .. } => *timeout_ms,
        };
        match ms {
            Some(ms) if ms > 0 => Duration::from_millis(ms),
            _ => DEFAULT_UI_TIMEOUT,
        }
    }

    /// A short human label for logs and telemetry.
    pub fn kind(&self) -> &'static str {
        match self {
            UiRequest::Select { .. } => "select",
            UiRequest::Confirm { .. } => "confirm",
            UiRequest::Input { .. } => "input",
            UiRequest::Editor { .. } => "editor",
        }
    }
}

/// The answer to a [`UiRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ui", rename_all = "snake_case")]
pub enum UiResponse {
    /// `select` / `input` / `editor` reply. `None` means empty input.
    Value { value: Option<String> },
    /// `confirm` reply.
    Confirmed { confirmed: bool },
    /// Dismissed, unanswered, timed out, or no UI available.
    Cancelled,
}

impl UiResponse {
    pub fn value(text: impl Into<String>) -> Self {
        UiResponse::Value {
            value: Some(text.into()),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, UiResponse::Cancelled)
    }

    /// Text payload, if this response carries one.
    pub fn text(&self) -> Option<&str> {
        match self {
            UiResponse::Value { value } => value.as_deref(),
            _ => None,
        }
    }

    /// `true` only for an explicit confirmation.
    pub fn confirmed(&self) -> bool {
        matches!(self, UiResponse::Confirmed { confirmed: true })
    }
}

/// The host's user-interaction surface.
///
/// Every method is non-panicking and resolves; a host that cannot serve a
/// request answers [`UiResponse::Cancelled`]. Implementations own the timeout.
#[async_trait]
pub trait HostUi: Send + Sync {
    /// Serve one blocking request.
    async fn request(&self, source: UiSource, request: UiRequest) -> UiResponse;

    /// Fire-and-forget notification. Never blocks, never fails loudly.
    fn notify(&self, source: UiSource, message: &str, level: NotifyLevel);

    /// Set or clear (`None`) a status entry in the host's status bar.
    fn set_status(&self, key: &str, text: Option<String>);

    // ---- Convenience wrappers over `request`. Callers normally use these. ----

    /// Host-initiated single choice. `None` means cancelled/timed out.
    async fn select(&self, title: &str, options: &[&str]) -> Option<String> {
        let owned: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        self.select_owned(title, owned).await
    }

    /// Plugin-initiated single choice.
    async fn select_from_plugin(&self, title: &str, options: &[&str]) -> Option<String> {
        let owned: Vec<String> = options.iter().map(|s| s.to_string()).collect();
        self.request(
            UiSource::Plugin,
            UiRequest::Select {
                title: title.to_string(),
                options: owned,
                timeout_ms: None,
            },
        )
        .await
        .text()
        .map(str::to_string)
    }

    async fn select_owned(&self, title: &str, options: Vec<String>) -> Option<String> {
        self.request(
            UiSource::Host,
            UiRequest::Select {
                title: title.to_string(),
                options,
                timeout_ms: None,
            },
        )
        .await
        .text()
        .map(str::to_string)
    }

    /// Host-initiated yes/no. `false` covers "no", cancelled, and timed out.
    async fn confirm(&self, title: &str, message: &str) -> bool {
        self.request(
            UiSource::Host,
            UiRequest::Confirm {
                title: title.to_string(),
                message: message.to_string(),
                timeout_ms: None,
            },
        )
        .await
        .confirmed()
    }

    /// Host-initiated free text.
    async fn input(&self, title: &str, placeholder: Option<&str>) -> Option<String> {
        self.request(
            UiSource::Host,
            UiRequest::Input {
                title: title.to_string(),
                placeholder: placeholder.map(str::to_string),
                timeout_ms: None,
            },
        )
        .await
        .text()
        .map(str::to_string)
    }
}

/// The absence of a UI: every dialog is declined, notifications are dropped.
///
/// This is the default for embedded and headless hosts, and it makes
/// "no UI" indistinguishable from "user cancelled" — which is exactly the
/// fail-closed property callers rely on.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullHostUi;

#[async_trait]
impl HostUi for NullHostUi {
    async fn request(&self, _source: UiSource, _request: UiRequest) -> UiResponse {
        UiResponse::Cancelled
    }

    fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}

    fn set_status(&self, _key: &str, _text: Option<String>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_timeout_applies_when_unset_or_zero() {
        let unset = UiRequest::Confirm {
            title: "t".into(),
            message: "m".into(),
            timeout_ms: None,
        };
        assert_eq!(unset.timeout(), DEFAULT_UI_TIMEOUT);

        let zero = UiRequest::Input {
            title: "t".into(),
            placeholder: None,
            timeout_ms: Some(0),
        };
        assert_eq!(zero.timeout(), DEFAULT_UI_TIMEOUT);

        let explicit = UiRequest::Select {
            title: "t".into(),
            options: vec![],
            timeout_ms: Some(1_500),
        };
        assert_eq!(explicit.timeout(), Duration::from_millis(1_500));
    }

    #[test]
    fn cancelled_is_never_confirmed_or_text() {
        let c = UiResponse::Cancelled;
        assert!(c.is_cancelled());
        assert!(!c.confirmed());
        assert_eq!(c.text(), None);
    }

    #[test]
    fn explicit_no_is_not_a_cancellation() {
        let no = UiResponse::Confirmed { confirmed: false };
        assert!(!no.is_cancelled());
        assert!(!no.confirmed());
    }

    #[tokio::test]
    async fn null_ui_fails_closed() {
        let ui = NullHostUi;
        assert!(!ui.confirm("t", "m").await);
        assert_eq!(ui.select("t", &["a", "b"]).await, None);
        assert_eq!(ui.input("t", None).await, None);
        // Fire-and-forget must not panic without a host.
        ui.notify(UiSource::Host, "hello", NotifyLevel::Info);
        ui.set_status("k", Some("v".into()));
        ui.set_status("k", None);
    }

    #[test]
    fn request_wire_shape_is_internally_tagged() {
        let json = serde_json::to_value(UiRequest::Select {
            title: "Allow?".into(),
            options: vec!["yes".into(), "no".into()],
            timeout_ms: Some(1000),
        })
        .unwrap();
        assert_eq!(json["ui"], "select");
        assert_eq!(json["title"], "Allow?");
        assert_eq!(json["options"][1], "no");

        // An absent timeout must not appear on the wire, so a panel can tell
        // "no deadline" from "deadline of 0".
        let json = serde_json::to_value(UiRequest::Input {
            title: "t".into(),
            placeholder: None,
            timeout_ms: None,
        })
        .unwrap();
        assert!(json.get("timeout_ms").is_none());
    }

    #[test]
    fn response_round_trips_through_json() {
        for res in [
            UiResponse::value("picked"),
            UiResponse::Confirmed { confirmed: true },
            UiResponse::Cancelled,
        ] {
            let json = serde_json::to_string(&res).unwrap();
            let back: UiResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(res, back);
        }
    }
}
