use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as TokioMutex;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct ReadFileTool {
    default_cwd: Option<PathBuf>,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait]
impl AgentTool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "read_file",
            "Read file contents with optional line offset and limit.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read."
                    }
                },
                "required": ["path"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'path' parameter")?;

        let p = Path::new(path_str);
        let target_path = if p.is_relative() {
            if let Some(ref cwd) = self.default_cwd {
                cwd.join(p)
            } else {
                p.to_path_buf()
            }
        } else {
            p.to_path_buf()
        };

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        if let Ok(meta) = fs::metadata(&target_path).await {
            let max_bytes = 10 * 1024 * 1024; // 10 MB ceiling
            if meta.len() > max_bytes && limit.is_none() {
                return Err(format!(
                    "File '{}' ({:.2} MB) exceeds maximum allowed single read limit of 10 MB. Please specify 'limit' or 'offset' to read in chunks.",
                    target_path.display(),
                    meta.len() as f64 / (1024.0 * 1024.0)
                ));
            }
        }

        let content = fs::read_to_string(&target_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {}", target_path.display(), e))?;

        if offset <= 1 && limit.is_none() {
            return Ok(content);
        }

        // Stream the requested slice without materializing every line.
        let start = offset.saturating_sub(1);
        let take = limit.unwrap_or(usize::MAX);
        let mut out = String::new();
        for line in content.lines().skip(start).take(take) {
            out.push_str(line);
            out.push('\n');
        }
        Ok(out.trim_end_matches('\n').to_string())
    }
}

#[derive(Default)]
pub struct WriteFileTool {
    default_cwd: Option<PathBuf>,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait]
impl AgentTool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "write_file",
            "Write content to a file on disk.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path."
                    },
                    "content": {
                        "type": "string",
                        "description": "File content string."
                    }
                },
                "required": ["path", "content"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'path' parameter")?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'content' parameter")?;

        // The run's token, so a cancelled run leaves the target untouched rather
        // than finishing the write it was in the middle of.
        let outcome = self
            .write_cancellable(path_str, content, &ctx.cancellation_token)
            .await;
        if let Some(sink) = ctx.event_sink.as_ref() {
            crate::file_change::emit_write(
                sink.as_ref(),
                path_str,
                Some(content.len()),
                outcome.is_err(),
            );
        }
        outcome
    }
}

impl WriteFileTool {
    /// Write `content` to `path`, parent directories included.
    ///
    /// The tool's own write core, shared with anything else that must land bytes
    /// the same way — a plan file, for instance, which needs the same atomicity
    /// and the same reason not to go through a tool.
    pub async fn write(&self, path_str: &str, content: &str) -> Result<String, String> {
        self.write_cancellable(path_str, content, &CancellationToken::new())
            .await
    }

    /// The same write, abandoning the attempt if `cancel` fires before the
    /// rename lands.
    ///
    /// Cancellation is checked *before* the commit, never after: the promise is
    /// that a stopped run leaves the target either untouched or fully written,
    /// which is only true if the rename is the last thing that can be skipped.
    /// The staged file goes with it — [`TempFileGuard`] removes it on any exit
    /// from the write, including the abrupt ones.
    pub async fn write_cancellable(
        &self,
        path_str: &str,
        content: &str,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let path = self.resolve(path_str);
        let parent = path.parent().unwrap_or_else(|| Path::new("."));

        // One writer at a time, per path. Parallel tool calls in a run, parallel
        // runs in one daemon, and a plugin's own call all share this table, so
        // two writers cannot interleave into one file and silently lose a write.
        let lock = file_lock_for(&path);
        let _permit = lock.lock().await;
        let had_original = path.exists();

        let outcome = tokio::select! {
            _ = cancel.cancelled() => {
                Err("Write cancelled before the file was replaced; the target is untouched".to_string())
            }
            res = self.stage_and_commit(&path, parent, content) => res,
        };

        // Still holding the permit: drop the table entry once nothing is queued.
        release_file_lock(&path, &lock);

        match outcome {
            Ok(()) => Ok(if had_original {
                format!(
                    "Successfully wrote {} bytes to {} (staged in {}, flushed to disk, then atomically \
                     renamed over the original; its permissions and its previous state are preserved \
                     up to the moment of the rename)",
                    content.len(),
                    path.display(),
                    parent.display()
                )
            } else {
                format!(
                    "Successfully created {} with {} bytes (staged, flushed to disk, then atomically renamed)",
                    path.display(),
                    content.len()
                )
            }),
            Err(err) => Err(if had_original {
                format!(
                    "{err}. Target file '{}' is untouched; the staged file was discarded.",
                    path.display()
                )
            } else {
                format!(
                    "{err}. '{}' was not created; the staged file was discarded.",
                    path.display()
                )
            }),
        }
    }

    /// Stage next to the target, flush, then rename onto it.
    ///
    /// The staging file lives in the target's own directory rather than a shared
    /// temp directory, which is what makes the rename atomic in the only way that
    /// matters: same filesystem, so there is no cross-device case to fall back
    /// from. It is hidden, so a watcher or a `git status` does not report a file
    /// that only exists for the microseconds this takes.
    async fn stage_and_commit(
        &self,
        path: &Path,
        parent: &Path,
        content: &str,
    ) -> Result<(), String> {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create parent directory: {}", e))?;

        let original_permissions = if path.exists() {
            fs::metadata(path).await.ok().map(|m| m.permissions())
        } else {
            None
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
        let staged = parent.join(format!(".{}_{}_{}.tmp", file_name, pid, nanos));
        let mut guard = TempFileGuard::new(staged.clone());

        {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&staged)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to create staging file '{}': {}",
                        staged.display(),
                        e
                    )
                })?;

            file.write_all(content.as_bytes())
                .await
                .map_err(|e| format!("Failed writing to staging file: {}", e))?;

            // Durability before the rename: a rename that lands before the bytes
            // do would leave a correctly-named empty file after a crash.
            file.sync_all()
                .await
                .map_err(|e| format!("Failed syncing staging file: {}", e))?;
        }

        if let Some(permissions) = original_permissions {
            let _ = tokio::fs::set_permissions(&staged, permissions).await;
        }

        tokio::fs::rename(&staged, path).await.map_err(|e| {
            format!(
                "Failed to atomically rename '{}' onto '{}': {}",
                staged.display(),
                path.display(),
                e
            )
        })?;
        guard.commit();
        Ok(())
    }

    /// The absolute path a call's `path` argument denotes.
    fn resolve(&self, path_str: &str) -> PathBuf {
        let p = Path::new(path_str);
        if p.is_relative() {
            match &self.default_cwd {
                Some(cwd) => cwd.join(p),
                None => p.to_path_buf(),
            }
        } else {
            p.to_path_buf()
        }
    }
}

/// Removes a staged file that was never committed.
///
/// Every failure path in `stage_and_commit` used to remember to delete its own
/// temp file, which is one `return` away from leaking one. This cannot forget —
/// including on cancellation, where the future is dropped mid-write.
struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One mutex per file path, shared by everything in this process.
///
/// Process-local by nature: two daemons, or a daemon and a TUI, do not
/// coordinate through this table. Within a process it is what makes "two agents
/// writing one file" a serialisation instead of a race.
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

/// The mutex for `path`, keyed by its resolved form so that two spellings of one
/// file (relative and absolute, a symlinked parent) cannot take two locks.
fn file_lock_for(path: &Path) -> Arc<TokioMutex<()>> {
    let key = lock_key(path);
    let mut locks = FILE_MUTATION_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone()
}

/// Best-effort hygiene: drop this path's entry when the caller is the only one
/// holding it. A queued writer holds its own clone, which keeps the entry alive,
/// so removing it here can never hand two writers two different mutexes.
///
/// Called while still holding the permit.
fn release_file_lock(path: &Path, lock: &Arc<TokioMutex<()>>) {
    let key = lock_key(path);
    let mut locks = FILE_MUTATION_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(current) = locks.get(&key) {
        if Arc::ptr_eq(current, lock) && Arc::strong_count(lock) == 2 {
            locks.remove(&key);
        }
    }
}

/// Canonicalize where possible, and fall back to canonicalizing the parent for a
/// file that does not exist yet (the common case for a create).
fn lock_key(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(resolved_parent) => resolved_parent.join(name),
            Err(_) => path.to_path_buf(),
        },
        _ => path.to_path_buf(),
    }
}
