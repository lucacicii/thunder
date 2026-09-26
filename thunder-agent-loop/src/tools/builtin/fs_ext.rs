//! Fast file-lookup and directory-listing tools built on the `ignore` crate
//! (fd's traversal core) and `globset` — in-process, .gitignore-aware.

use crate::tools::builtin::search::{build_glob_set, build_walker, relative_display};
use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use async_trait::async_trait;
use ignore::WalkState;
use parking_lot::Mutex;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Max results (pi's find DEFAULT_LIMIT).
const DEFAULT_FIND_LIMIT: usize = 1000;
/// Hard cap on output (pi's DEFAULT_MAX_BYTES).
const MAX_OUTPUT_BYTES: usize = 50 * 1024;
/// Max entries listed per directory.
const DEFAULT_LS_LIMIT: usize = 1000;

fn resolve_root(raw: &str, default_cwd: &Option<PathBuf>) -> Result<PathBuf, String> {
    let joined = default_cwd
        .as_ref()
        .map(|cwd| cwd.join(raw))
        .unwrap_or_else(|| PathBuf::from(raw));
    if !joined.exists() {
        return Err(format!("Path not found: {}", joined.display()));
    }
    Ok(joined)
}

// ────────────────────────────────────────────────────────────
// find
// ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct FindTool {
    default_cwd: Option<PathBuf>,
}

impl FindTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

struct FindShared {
    results: Mutex<Vec<String>>,
    result_count: AtomicUsize,
    total_bytes: AtomicUsize,
    truncated: AtomicBool,
    limit: usize,
}

impl FindShared {
    fn at_capacity(&self) -> bool {
        self.result_count.load(Ordering::Relaxed) >= self.limit
            || self.total_bytes.load(Ordering::Relaxed) >= MAX_OUTPUT_BYTES
    }
}

#[async_trait]
impl AgentTool for FindTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "find",
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search in (default: current directory)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 1000)"
                    }
                },
                "required": ["pattern"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'pattern'".to_string())?;
        if pattern.trim().is_empty() {
            return Err("Pattern must not be empty".to_string());
        }

        let path_raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = resolve_root(path_raw, &self.default_cwd)?;
        if root.is_file() {
            return Ok(root.display().to_string());
        }

        let glob_set = build_glob_set(pattern)?
            .ok_or_else(|| "Pattern must not be empty".to_string())?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).max(1))
            .unwrap_or(DEFAULT_FIND_LIMIT);

        let shared = Arc::new(FindShared {
            results: Mutex::new(Vec::new()),
            result_count: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
            truncated: AtomicBool::new(false),
            limit,
        });
        let cancel = ctx.cancellation_token.clone();

        let (mut output, truncated) = tokio::task::spawn_blocking(move || {
            let walker = build_walker(&root).build_parallel();
            walker.run(|| {
                let shared = shared.clone();
                let glob_set = glob_set.clone();
                let root = root.clone();
                let cancel = cancel.clone();
                Box::new(move |entry| {
                    if shared.at_capacity() || cancel.is_cancelled() {
                        shared.truncated.store(true, Ordering::Relaxed);
                        return WalkState::Quit;
                    }
                    let Ok(entry) = entry else { return WalkState::Continue };
                    let is_file = entry.file_type().is_some_and(|t| t.is_file());
                    if !is_file {
                        return WalkState::Continue;
                    }
                    if glob_set.is_match(entry.path()) {
                        let rel = relative_display(entry.path(), &root);
                        let mut results = shared.results.lock();
                        if results.len() < shared.limit {
                            shared.result_count.fetch_add(1, Ordering::Relaxed);
                            shared
                                .total_bytes
                                .fetch_add(rel.len() + 1, Ordering::Relaxed);
                            results.push(rel);
                        }
                    }
                    WalkState::Continue
                })
            });
            let truncated = shared.truncated.load(Ordering::Relaxed);
            let mut results = std::mem::take(&mut *shared.results.lock());
            results.sort();
            (results.join("\n"), truncated)
        })
        .await
        .map_err(|e| format!("Find task failed: {}", e))?;

        if output.is_empty() {
            return Ok(format!("No files found matching '{}'", pattern));
        }
        if truncated {
            output.push_str("\n\n[Output truncated — narrow the pattern or pass a smaller 'limit']");
        }
        Ok(output)
    }
}

// ────────────────────────────────────────────────────────────
// ls
// ────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct ListDirTool {
    default_cwd: Option<PathBuf>,
}

impl ListDirTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait]
impl AgentTool for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "ls",
            "List directory entries (single level). Directories are shown with a trailing '/'. Sorted: directories first, then files, alphabetically. Hidden files included.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: current directory)"
                    }
                },
                "required": []
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path_raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let root = resolve_root(path_raw, &self.default_cwd)?;
        if !root.is_dir() {
            return Err(format!("Not a directory: {}", root.display()));
        }

        let mut dirs: Vec<String> = Vec::new();
        let mut files: Vec<(String, u64)> = Vec::new();

        let mut entries = std::fs::read_dir(&root)
            .map_err(|e| format!("Failed to read directory '{}': {}", root.display(), e))?;

        let mut truncated = false;
        while let Some(entry) = entries.next().transpose().map_err(|e| {
            format!("Failed to read directory '{}': {}", root.display(), e)
        })? {
            if dirs.len() + files.len() >= DEFAULT_LS_LIMIT {
                truncated = true;
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                dirs.push(format!("{}/", name));
            } else {
                files.push((name, meta.len()));
            }
        }

        dirs.sort();
        files.sort();

        let mut output = String::new();
        for d in &dirs {
            output.push_str(d);
            output.push('\n');
        }
        for (name, size) in &files {
            output.push_str(&format!("{} ({} bytes)\n", name, size));
        }
        if output.is_empty() {
            return Ok(format!("Directory is empty: {}", root.display()));
        }
        if truncated {
            output.push_str("\n[Output truncated at 1000 entries]");
        }
        Ok(output.trim_end_matches('\n').to_string())
    }
}
