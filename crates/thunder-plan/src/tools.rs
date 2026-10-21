use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thunder_protocol::ExecutionContext;
use tokio::io::AsyncWriteExt;

static ATOMIC_PLAN_SEQ: AtomicU64 = AtomicU64::new(1);

/// Callback invoked when `exit_plan_mode` successfully completes, flipping the host session mode to `yolo` or `ask`.
pub type ModeFlipCallback = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub fn plan_file_path(workspace_cwd: &Path, session_id: &str) -> PathBuf {
    let clean_session: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    workspace_cwd
        .join(".thunder")
        .join("plans")
        .join(format!("{clean_session}.md"))
}

pub async fn handle_write_plan(
    arguments: &Value,
    ctx: &ExecutionContext,
) -> Result<String, String> {
    let content = arguments
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'content'".to_string())?;

    if content.trim().is_empty() {
        return Err("Parameter 'content' must not be empty".to_string());
    }

    let target = plan_file_path(&ctx.workspace_cwd, &ctx.session_id);
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await.map_err(|e| {
        format!(
            "Failed to create plan directory '{}': {e}",
            parent.display()
        )
    })?;

    let seq = ATOMIC_PLAN_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let tmp_path = parent.join(format!(".plan_tmp_{pid}_{seq}"));

    {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .await
            .map_err(|e| format!("Failed creating temporary plan file: {e}"))?;
        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed writing plan content: {e}"))?;
        file.sync_all()
            .await
            .map_err(|e| format!("Failed syncing plan file: {e}"))?;
    }

    tokio::fs::rename(&tmp_path, &target)
        .await
        .map_err(|e| format!("Failed atomically renaming plan file: {e}"))?;

    Ok(format!(
        "Plan successfully saved to {} ({} bytes). You can now call exit_plan_mode when ready for execution.",
        target.display(),
        content.len()
    ))
}

pub async fn handle_exit_plan_mode(
    arguments: &Value,
    ctx: &ExecutionContext,
    flip_cb: Option<&ModeFlipCallback>,
) -> Result<String, String> {
    let target = plan_file_path(&ctx.workspace_cwd, &ctx.session_id);
    if !target.exists() {
        return Err(format!(
            "Cannot exit plan mode: no plan file found at '{}'. Call write_plan first.",
            target.display()
        ));
    }

    let content = tokio::fs::read_to_string(&target)
        .await
        .map_err(|e| format!("Failed reading plan file '{}': {e}", target.display()))?;

    if content.trim().is_empty() {
        return Err("Cannot exit plan mode: plan file is empty. Call write_plan with a concrete plan first.".to_string());
    }

    let explanation = arguments
        .get("explanation")
        .and_then(|v| v.as_str())
        .unwrap_or("Plan completed and ready for implementation.");

    if let Some(cb) = flip_cb {
        cb(&ctx.session_id, explanation);
    }

    Ok(format!(
        "Plan mode exited successfully! Plan at '{}' is approved. You can now execute file writes and build steps.",
        target.display()
    ))
}
