//! Reporting what a tool did to the workspace, as a `file_change` custom event.
//!
//! This used to be a middleware layer, which is how it ended up running *around*
//! tools: it snapshotted `git status`, guessed from the arguments what the tool
//! was about to do, and reconciled the two afterwards. The tools themselves know
//! exactly what they touched, so they report it and this module holds the shared
//! shape. Same event, same payload keys, one layer less.
//!
//! The kernel carries the event without knowing what a file is: it sees
//! `AgentEvent::Custom { kind: "file_change" }` and forwards it verbatim.

use std::collections::HashSet;
use std::path::Path;
use thunder_agent_types::tool::ToolEventSink;

/// The event kind a host matches on. Part of the wire contract — do not rename.
pub const FILE_CHANGE: &str = "file_change";

/// Report one file a tool wrote (or failed to write).
pub fn emit_write(sink: &dyn ToolEventSink, path: &str, bytes: Option<usize>, is_error: bool) {
    sink.emit_custom(
        FILE_CHANGE,
        serde_json::json!({
            "path": path,
            "action": if is_error { "failed" } else { "written" },
            "bytes": bytes,
            "tool": "write_file",
        }),
    );
}

/// `git status --porcelain` entries, or an empty set when the workspace is not a
/// git checkout (or git is unavailable).
pub async fn git_status_snapshot(workspace: &Path) -> HashSet<String> {
    let output = tokio::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| line.len() > 3)
            .map(str::to_string)
            .collect(),
        _ => HashSet::new(),
    }
}

/// Report every workspace change that appeared between two snapshots.
///
/// A shell command's file effects are invisible from its arguments, so this is
/// the one place a snapshot is still the right tool. Called only by `bash`, and
/// only after it ran — a refused command never reaches the tool at all.
pub async fn emit_bash_changes(
    sink: &dyn ToolEventSink,
    workspace: &Path,
    before: &HashSet<String>,
) {
    let after = git_status_snapshot(workspace).await;
    for entry in &after {
        if before.contains(entry) {
            continue;
        }
        let path = if entry.len() > 3 {
            entry[3..].trim().to_string()
        } else {
            entry.clone()
        };
        let action = if entry.starts_with("??") {
            "created"
        } else if entry.starts_with('D') || entry.contains(" D") {
            "deleted"
        } else {
            "modified"
        };
        sink.emit_custom(
            FILE_CHANGE,
            serde_json::json!({
                "path": path,
                "action": action,
                "tool": "bash",
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use thunder_agent_types::tool::ToolEventSink;

    #[derive(Default, Debug)]
    struct Recorder {
        events: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl ToolEventSink for Recorder {
        fn emit_custom(&self, kind: &str, payload: serde_json::Value) {
            self.events
                .lock()
                .unwrap()
                .push((kind.to_string(), payload));
        }
    }

    #[test]
    fn a_write_reports_the_path_and_the_action() {
        let recorder = Arc::new(Recorder::default());
        emit_write(recorder.as_ref(), "src/main.rs", Some(12), false);

        let events = recorder.events.lock().unwrap();
        let (kind, payload) = &events[0];
        assert_eq!(kind, FILE_CHANGE);
        assert_eq!(payload["path"], serde_json::json!("src/main.rs"));
        assert_eq!(payload["action"], serde_json::json!("written"));
        assert_eq!(payload["bytes"], serde_json::json!(12));
        assert_eq!(payload["tool"], serde_json::json!("write_file"));
    }

    #[test]
    fn a_failed_write_says_so() {
        let recorder = Arc::new(Recorder::default());
        emit_write(recorder.as_ref(), "src/main.rs", Some(12), true);
        assert_eq!(
            recorder.events.lock().unwrap()[0].1["action"],
            serde_json::json!("failed")
        );
    }

    #[tokio::test]
    async fn a_bash_delta_reports_only_what_changed() {
        let recorder = Arc::new(Recorder::default());
        let workspace = std::env::temp_dir().join(format!("thunder_fc_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();

        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&workspace)
                .status()
                .map(|s| s.success())
                .unwrap_or(false),
            "git is required for this test"
        );

        let before = git_status_snapshot(&workspace).await;
        std::fs::write(workspace.join("new.txt"), "x").unwrap();
        emit_bash_changes(recorder.as_ref(), &workspace, &before).await;

        let events = recorder.events.lock().unwrap();
        assert_eq!(events.len(), 1, "only the new file is reported");
        assert_eq!(events[0].1["path"], serde_json::json!("new.txt"));
        assert_eq!(events[0].1["action"], serde_json::json!("created"));

        let _ = std::fs::remove_dir_all(&workspace);
    }
}
