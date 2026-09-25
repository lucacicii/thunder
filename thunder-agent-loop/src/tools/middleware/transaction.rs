use crate::tools::middleware::telemetry::SystemNotice;
use crate::tools::middleware::{ToolHandler, ToolMiddleware};
use crate::types::message::ToolCall;
use crate::types::tool::{ToolExecutionContext, ToolExecutionResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as TokioMutex;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Cross-agent/task per-file mutation mutex map (analogous to Pi's file-mutation-queue).
/// Serializes concurrent write operations aimed at the exact same normalized file path,
/// while allowing concurrent writes to distinct files to proceed completely in parallel.
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

fn get_file_mutation_lock(target_path: &Path) -> Arc<TokioMutex<()>> {
    let normalized = if let Ok(c) = target_path.canonicalize() {
        c
    } else if let Some(parent) = target_path.parent() {
        if let Ok(parent_c) = parent.canonicalize() {
            if let Some(name) = target_path.file_name() {
                parent_c.join(name)
            } else {
                target_path.to_path_buf()
            }
        } else {
            target_path.to_path_buf()
        }
    } else {
        target_path.to_path_buf()
    };

    let mut locks = FILE_MUTATION_LOCKS.lock().unwrap();
    locks
        .entry(normalized)
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone()
}

/// RAII Guard that automatically removes an uncommitted temporary file on drop.
pub struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    pub fn commit(&mut self) {
        self.committed = true;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed && self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Transaction & Atomic I/O Middleware.
///
/// Ensures all file write operations are routed through a shadow temporary file
/// in `.arp/tmp/`, physically flushed to disk, and atomically renamed onto the target.
/// In case of error, timeout, or cancellation, uncommitted temporary files are cleanly discarded
/// and the LLM receives an omniscient telemetry report confirming target file integrity.
#[derive(Clone)]
pub struct TransactionMiddleware {
    workspace_root: PathBuf,
    temp_dir: PathBuf,
}

impl TransactionMiddleware {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let temp_dir = ws.join(".arp").join("tmp");
        Self {
            workspace_root: ws,
            temp_dir,
        }
    }

    pub fn default_for_workspace(ws: Option<PathBuf>) -> Self {
        let root = ws.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Self::new(root)
    }

    pub fn temp_dir(&self) -> &Path {
        &self.temp_dir
    }

    /// Initializes `.arp/tmp/` directory, writes `.gitignore`, and purges stale temp files older than `max_age`.
    pub async fn init_and_clean_stale(&self, max_age: Duration) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.temp_dir).await?;

        // Ensure .arp/tmp/.gitignore exists to keep git trees clean
        let gitignore_path = self.temp_dir.join(".gitignore");
        if !gitignore_path.exists() {
            let _ = tokio::fs::write(&gitignore_path, "*\n!.gitignore\n").await;
        }

        self.clean_stale_temp_files(max_age).await?;
        Ok(())
    }

    /// Scans `.arp/tmp/` and purges any `.tmp` files older than `max_age`.
    pub async fn clean_stale_temp_files(&self, max_age: Duration) -> std::io::Result<()> {
        if !self.temp_dir.exists() {
            return Ok(());
        }

        let mut entries = tokio::fs::read_dir(&self.temp_dir).await?;
        let now = SystemTime::now();

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "tmp" {
                        if let Ok(meta) = entry.metadata().await {
                            if let Ok(modified) = meta.modified() {
                                if let Ok(age) = now.duration_since(modified) {
                                    if age > max_age {
                                        let _ = tokio::fs::remove_file(&path).await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn generate_temp_path(&self, target_path: &Path) -> PathBuf {
        let file_stem = target_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .replace(['/', '\\', ' '], "_");

        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);

        let filename = format!("{}.{}_{}_{}.tmp", file_stem, nanos, pid, counter);
        self.temp_dir.join(filename)
    }

    async fn execute_atomic_write(
        &self,
        target_path: PathBuf,
        content: &str,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
    ) -> Result<(String, SystemNotice), (String, SystemNotice)> {
        // Acquire per-file mutation lock to serialize concurrent writes from parallel agents
        let file_lock = get_file_mutation_lock(&target_path);
        let _file_permit = file_lock.lock().await;

        let _ = tokio::fs::create_dir_all(&self.temp_dir).await;
        let temp_path = self.generate_temp_path(&target_path);
        let mut guard = TempFileGuard::new(temp_path.clone());

        // Snapshot original metadata & permissions to preserve them upon rename
        let original_meta = if target_path.exists() {
            tokio::fs::metadata(&target_path).await.ok()
        } else {
            None
        };
        let had_original = original_meta.is_some();
        let original_permissions = original_meta.map(|m| m.permissions());

        let timeout_dur = timeout.unwrap_or(Duration::from_secs(30));

        let write_future = async {
            // Ensure parent directory of target exists
            if let Some(parent) = target_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("Failed to create parent directory '{}': {}", parent.display(), e))?;
            }

            // Write to shadow temp file
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .await
                .map_err(|e| format!("Failed to open temp shadow file '{}': {}", temp_path.display(), e))?;

            file.write_all(content.as_bytes())
                .await
                .map_err(|e| format!("Failed writing bytes to temp file: {}", e))?;

            // Hardware fsync flush
            file.sync_all()
                .await
                .map_err(|e| format!("Failed to sync temp file to disk: {}", e))?;
            drop(file);

            // Restore original file permissions if existed (e.g. executable 0755)
            if let Some(perm) = original_permissions {
                let _ = tokio::fs::set_permissions(&temp_path, perm).await;
            }

            // Atomic rename
            match tokio::fs::rename(&temp_path, &target_path).await {
                Ok(_) => {
                    guard.commit();
                    Ok(())
                }
                Err(err) if err.kind() == std::io::ErrorKind::CrossesDevices => {
                    // Fallback for cross-device mount links: use hidden temp in target's parent dir
                    if let Some(parent) = target_path.parent() {
                        let fallback_name = format!(".{}.tmp", temp_path.file_name().unwrap().to_string_lossy());
                        let fallback_temp = parent.join(fallback_name);
                        let mut fallback_guard = TempFileGuard::new(fallback_temp.clone());

                        tokio::fs::copy(&temp_path, &fallback_temp)
                            .await
                            .map_err(|e| format!("Cross-device copy failed: {}", e))?;

                        tokio::fs::rename(&fallback_temp, &target_path)
                            .await
                            .map_err(|e| format!("Cross-device fallback rename failed: {}", e))?;

                        fallback_guard.commit();
                        guard.commit();
                        let _ = tokio::fs::remove_file(&temp_path).await;
                        Ok(())
                    } else {
                        Err(format!("Rename failed across devices: {}", err))
                    }
                }
                Err(err) => Err(format!("Atomic rename failed: {}", err)),
            }
        };

        // Enforce cancellation and timeout monitoring
        let result = tokio::select! {
            _ = ctx.cancellation_token.cancelled() => {
                Err("Write operation cancelled by cancellation signal".to_string())
            }
            res = tokio::time::timeout(timeout_dur, write_future) => {
                match res {
                    Ok(inner) => inner,
                    Err(_) => Err(format!("Write operation timed out after {:?}", timeout_dur)),
                }
            }
        };

        match result {
            Ok(_) => {
                let ground_truth = if had_original {
                    format!(
                        "Written {} bytes to '{}' via atomic rename. Original file safely replaced and permissions preserved.",
                        content.len(),
                        target_path.display()
                    )
                } else {
                    format!(
                        "Created new file '{}' with {} bytes via atomic shadow staging.",
                        target_path.display(),
                        content.len()
                    )
                };

                let notice = SystemNotice::new(
                    "Transaction",
                    "Atomic shadow write & rename completed",
                    ground_truth.clone(),
                );
                Ok((ground_truth, notice))
            }
            Err(err_msg) => {
                // TempFileGuard drops here and deletes temp_path
                let ground_truth = if had_original {
                    format!(
                        "Target file '{}' remains 100% UNTOUCHED at original state (no data corruption). Shadow temp file discarded.",
                        target_path.display()
                    )
                } else {
                    format!(
                        "Target file '{}' was not created. Shadow temp file discarded.",
                        target_path.display()
                    )
                };

                let notice = SystemNotice::new(
                    "Transaction",
                    "Write aborted due to error or cancellation",
                    ground_truth,
                )
                .with_guidance("You may safely retry this write operation or split content into smaller chunks.");

                Err((err_msg, notice))
            }
        }
    }
}

#[async_trait]
impl ToolMiddleware for TransactionMiddleware {
    fn name(&self) -> &str {
        "TransactionMiddleware"
    }

    async fn handle(
        &self,
        call: &ToolCall,
        ctx: &ToolExecutionContext,
        timeout: Option<Duration>,
        next: Arc<dyn ToolHandler>,
    ) -> ToolExecutionResult {
        // Intercept write_file calls for atomic staging
        if call.function.name == "write_file" {
            let start = Instant::now();

            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&call.function.arguments);
            let args = match parsed {
                Ok(v) => v,
                Err(err) => {
                    return ToolExecutionResult::error(
                        format!("Error parsing arguments for write_file: {}", err),
                        start.elapsed(),
                    );
                }
            };

            let path_str = args.get("path").and_then(|v| v.as_str());
            let content_str = args.get("content").and_then(|v| v.as_str());

            if let (Some(path_str), Some(content_str)) = (path_str, content_str) {
                let p = Path::new(path_str);
                let target_path = if p.is_relative() {
                    self.workspace_root.join(p)
                } else {
                    p.to_path_buf()
                };

                match self.execute_atomic_write(target_path, content_str, ctx, timeout).await {
                    Ok((output_msg, notice)) => {
                        return ToolExecutionResult::success(output_msg, start.elapsed()).with_telemetry(notice);
                    }
                    Err((err_msg, notice)) => {
                        return ToolExecutionResult::error(err_msg, start.elapsed()).with_telemetry(notice);
                    }
                }
            }
        }

        // Pass through non-write tools to the next middleware
        next.handle(call, ctx, timeout).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct DummyTerminal;

    #[async_trait]
    impl ToolHandler for DummyTerminal {
        async fn handle(
            &self,
            _call: &ToolCall,
            _ctx: &ToolExecutionContext,
            _timeout: Option<Duration>,
        ) -> ToolExecutionResult {
            ToolExecutionResult::success("pass-through".to_string(), Duration::from_millis(1))
        }
    }

    #[tokio::test]
    async fn test_atomic_write_success_and_telemetry() {
        let test_root = std::env::temp_dir().join(format!("thunder_atomic_test_{}", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::create_dir_all(&test_root);
        let ws = test_root.clone();
        let middleware = TransactionMiddleware::new(&ws);

        let target_file = ws.join("test.txt");
        let call = ToolCall::new_function(
            "call_write_1",
            "write_file",
            serde_json::json!({
                "path": "test.txt",
                "content": "Hello Atomic Thunder!"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_write_1".to_string(),
            turn: 1,
            cancellation_token: CancellationToken::new(),
        };

        let result = middleware
            .handle(&call, &ctx, None, Arc::new(DummyTerminal))
            .await;

        assert!(!result.is_error);
        assert!(target_file.exists());
        let content = std::fs::read_to_string(&target_file).expect("read");
        assert_eq!(content, "Hello Atomic Thunder!");
        assert!(result.output.contains("[System Telemetry: Transaction"));
        assert!(result.output.contains("Atomic shadow write & rename completed"));
        let _ = std::fs::remove_dir_all(&test_root);
    }

    #[tokio::test]
    async fn test_atomic_write_cancellation_preserves_original() {
        let test_root = std::env::temp_dir().join(format!("thunder_atomic_test_cancel_{}", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::create_dir_all(&test_root);
        let ws = test_root.clone();
        let middleware = TransactionMiddleware::new(&ws);

        let target_file = ws.join("existing.txt");
        std::fs::write(&target_file, "Original Important Content").expect("seed file");

        let cancel_token = CancellationToken::new();
        cancel_token.cancel(); // Pre-cancel to simulate cancellation during write

        let call = ToolCall::new_function(
            "call_write_2",
            "write_file",
            serde_json::json!({
                "path": "existing.txt",
                "content": "Corrupted partial text"
            })
            .to_string(),
        );

        let ctx = ToolExecutionContext {
            tool_call_id: "call_write_2".to_string(),
            turn: 1,
            cancellation_token: cancel_token,
        };

        let result = middleware
            .handle(&call, &ctx, None, Arc::new(DummyTerminal))
            .await;

        assert!(result.is_error);
        // Original content MUST be untouched!
        let content = std::fs::read_to_string(&target_file).expect("read");
        assert_eq!(content, "Original Important Content");
        assert!(result.output.contains("remains 100% UNTOUCHED"));
        let _ = std::fs::remove_dir_all(&test_root);
    }

    #[tokio::test]
    async fn test_concurrent_writes_are_serialized_safely() {
        let test_root = std::env::temp_dir().join(format!("thunder_atomic_concurrent_{}", SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = std::fs::create_dir_all(&test_root);
        let ws = test_root.clone();
        let middleware = Arc::new(TransactionMiddleware::new(&ws));

        let mut handles = Vec::new();
        for i in 0..10 {
            let mw = Arc::clone(&middleware);
            let handle = tokio::spawn(async move {
                let call = ToolCall::new_function(
                    format!("call_concurrent_{i}"),
                    "write_file",
                    serde_json::json!({
                        "path": "shared_output.txt",
                        "content": format!("Write version {i}\n")
                    })
                    .to_string(),
                );
                let ctx = ToolExecutionContext {
                    tool_call_id: format!("call_concurrent_{i}"),
                    turn: 1,
                    cancellation_token: CancellationToken::new(),
                };
                mw.handle(&call, &ctx, None, Arc::new(DummyTerminal)).await
            });
            handles.push(handle);
        }

        for handle in handles {
            let res = handle.await.expect("join");
            assert!(!res.is_error, "Concurrent write failed: {}", res.output);
        }

        let target_file = ws.join("shared_output.txt");
        assert!(target_file.exists());
        let final_content = std::fs::read_to_string(&target_file).expect("read");
        assert!(final_content.starts_with("Write version "));
        let _ = std::fs::remove_dir_all(&test_root);
    }
}
