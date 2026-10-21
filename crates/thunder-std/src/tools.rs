use grep_regex::RegexMatcherBuilder;
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use ignore::{WalkBuilder, WalkState};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex};
use std::time::Duration;
use thunder_executor::{
    drain_stream_with_flood_guard, execute_shell_command, DEFAULT_MAX_OUTPUT_BYTES,
};
use thunder_protocol::{
    ContextEnvelopeMeta, ExecutionContext, McpCallToolResult, McpToolCallParams, McpToolInfo,
    ToolExecutionError,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Mutex as TokioMutex;

static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(1);

const DEFAULT_MATCH_LIMIT: usize = 100;
const MAX_LINE_LENGTH: usize = 500;
const MAX_SEARCH_OUTPUT_BYTES: usize = 50 * 1024;
const DEFAULT_FIND_LIMIT: usize = 1000;
const DEFAULT_LS_LIMIT: usize = 1000;

/// Process-wide per-path write lock table preventing concurrent writes to the same file from racing.
static FILE_MUTATION_LOCKS: LazyLock<StdMutex<HashMap<PathBuf, Arc<TokioMutex<()>>>>> =
    LazyLock::new(|| StdMutex::new(HashMap::new()));

pub fn file_lock_for(path: &Path) -> Arc<TokioMutex<()>> {
    let key = lock_key(path);
    let mut locks = FILE_MUTATION_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(TokioMutex::new(())))
        .clone()
}

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

/// Return the MCP tool definitions exposed by `thunder-std` (§2.2).
pub fn std_tool_definitions() -> Vec<McpToolInfo> {
    vec![
        McpToolInfo {
            name: "read_file".to_string(),
            description: Some(
                "Read the contents of a file (resolves relative paths against _meta.workspace_cwd)."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "format": "file-path",
                        "description": "Path to the file to read (relative to workspace_cwd or absolute)."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Optional 1-indexed starting line number."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Optional maximum number of lines to read."
                    }
                },
                "required": ["path"]
            }),
        },
        McpToolInfo {
            name: "write_file".to_string(),
            description: Some(
                "Atomically write content to a file with per-path locking, creating parent directories if needed."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "format": "file-path",
                        "description": "Path to the file to write (relative to workspace_cwd or absolute)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Full text content to write."
                    }
                },
                "required": ["path", "content"]
            }),
        },
        McpToolInfo {
            name: "edit_block".to_string(),
            description: Some(
                "Replace an exact, unique text block in a file atomically with per-path locking.".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "format": "file-path",
                        "description": "Path to the file to edit."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Exact existing text block (must match uniquely)."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text block."
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        },
        McpToolInfo {
            name: "bash".to_string(),
            description: Some(
                "Execute a bash command inside an isolated process group with 64KB flood protection."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute."
                    },
                    "cwd": {
                        "type": "string",
                        "format": "directory-path",
                        "description": "Optional working directory override."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Optional timeout override in milliseconds."
                    }
                },
                "required": ["command"]
            }),
        },
        McpToolInfo {
            name: "grep".to_string(),
            description: Some(
                "Search file contents for a pattern using in-process ripgrep (respects .gitignore)."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex or literal string)."
                    },
                    "path": {
                        "type": "string",
                        "format": "file-path",
                        "description": "Directory or file to search (default: workspace_cwd)."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Optional file glob filter, e.g. '*.rs'."
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)."
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string instead of regex (default: false)."
                    },
                    "context": {
                        "type": "integer",
                        "description": "Lines of context before/after each match (default: 0)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)."
                    }
                },
                "required": ["pattern"]
            }),
        },
        McpToolInfo {
            name: "find".to_string(),
            description: Some(
                "Search for files by glob pattern (respects .gitignore).".to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern to match files, e.g. '*.rs' or 'src/**/*.toml'."
                    },
                    "path": {
                        "type": "string",
                        "format": "directory-path",
                        "description": "Directory to search in (default: workspace_cwd)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results (default: 1000)."
                    }
                },
                "required": ["pattern"]
            }),
        },
        McpToolInfo {
            name: "ls".to_string(),
            description: Some(
                "List directory entries (single level, directories first with trailing '/', then files with sizes)."
                    .to_string(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "format": "directory-path",
                        "description": "Directory to list (default: workspace_cwd)."
                    }
                }
            }),
        },
    ]
}

/// Resolve base `ExecutionContext` from the request's `_meta` Context Envelope (§3.1).
pub fn resolve_context(meta: Option<&ContextEnvelopeMeta>) -> ExecutionContext {
    let fallback_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match meta {
        Some(m) => ExecutionContext {
            session_id: m.session_id.clone(),
            workspace_cwd: m.workspace_cwd.clone(),
            timeout: Duration::from_millis(m.timeout_ms.max(100)),
        },
        None => ExecutionContext {
            session_id: "default-session".to_string(),
            workspace_cwd: fallback_cwd,
            timeout: Duration::from_secs(30),
        },
    }
}

/// Resolve a potentially relative path against `workspace_cwd` (§3.1).
pub fn resolve_path(workspace_cwd: &Path, raw_path: &str) -> PathBuf {
    let p = Path::new(raw_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_cwd.join(p)
    }
}

/// Dispatch a `tools/call` invocation in `thunder-std`.
pub async fn dispatch_tool_call(params: McpToolCallParams) -> McpCallToolResult {
    let ctx = resolve_context(params.meta.as_ref());
    let outcome = match params.name.as_str() {
        "read_file" => handle_read_file(&params.arguments, &ctx).await,
        "write_file" => handle_write_file(&params.arguments, &ctx).await,
        "edit_block" => handle_edit_block(&params.arguments, &ctx).await,
        "bash" => handle_bash(&params.arguments, &ctx).await,
        "grep" => handle_grep(&params.arguments, &ctx).await,
        "find" => handle_find(&params.arguments, &ctx).await,
        "ls" => handle_ls(&params.arguments, &ctx).await,
        other => Err(format!("Unknown thunder-std tool: '{other}'")),
    };

    match outcome {
        Ok(text) => McpCallToolResult::success_text(text),
        Err(err) => McpCallToolResult::error_text(err),
    }
}

const MAX_SINGLE_READ_BYTES: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_STREAMED_OUTPUT_BYTES: usize = 512 * 1024; // 512 KB

async fn handle_read_file(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
    let target = resolve_path(&ctx.workspace_cwd, raw_path);

    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let meta = tokio::fs::metadata(&target)
        .await
        .map_err(|e| format!("Failed to read file metadata '{}': {e}", target.display()))?;

    let sliced = if meta.len() > MAX_SINGLE_READ_BYTES {
        // Streamed line-by-line reading for files > 10MB to avoid OOM
        read_streamed(&target, offset, limit).await?
    } else {
        let content = tokio::fs::read_to_string(&target)
            .await
            .map_err(|e| format!("Failed to read '{}': {e}", target.display()))?;

        if offset > 1 || limit.is_some() {
            let lines: Vec<&str> = content.lines().collect();
            let start = (offset - 1).min(lines.len());
            let end = match limit {
                Some(l) => (start + l).min(lines.len()),
                None => lines.len(),
            };
            lines[start..end].join("\n")
        } else {
            content
        }
    };

    let guarded = drain_stream_with_flood_guard(
        std::io::Cursor::new(sliced.into_bytes()),
        DEFAULT_MAX_OUTPUT_BYTES,
        &ctx.session_id,
        "read_file",
    )
    .await
    .map_err(|e| format!("Flood guard error: {e}"))?;

    Ok(guarded.text)
}

async fn read_streamed(
    target_path: &Path,
    offset: usize,
    limit: Option<usize>,
) -> Result<String, String> {
    let file = tokio::fs::File::open(target_path)
        .await
        .map_err(|e| format!("Failed to read file '{}': {e}", target_path.display()))?;
    let mut lines = tokio::io::BufReader::new(file).lines();
    let take = limit.unwrap_or(usize::MAX);
    let mut out = String::new();
    let mut line_no = 0usize;
    let mut taken = 0usize;
    let mut truncated = false;

    while taken < take {
        let line = lines
            .next_line()
            .await
            .map_err(|e| format!("Failed to read file '{}': {e}", target_path.display()))?;
        let Some(line) = line else { break };
        line_no += 1;
        if line_no < offset {
            continue;
        }
        if out.len() + line.len() + 1 > MAX_STREAMED_OUTPUT_BYTES {
            truncated = true;
            break;
        }
        out.push_str(&line);
        out.push('\n');
        taken += 1;
    }

    if truncated {
        out.push_str("…[truncated: output exceeded the 512 KiB streaming budget; re-read with a smaller limit/offset window]");
        return Ok(out);
    }

    Ok(out.trim_end_matches('\n').to_string())
}

async fn handle_write_file(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'content'".to_string())?;

    let target = resolve_path(&ctx.workspace_cwd, raw_path);
    let lock = file_lock_for(&target);
    let _permit = lock.lock().await;
    let res = atomic_write(&target, content.as_bytes()).await;
    release_file_lock(&target, &lock);
    res?;

    Ok(format!(
        "Successfully wrote {} bytes to {}",
        content.len(),
        target.display()
    ))
}

async fn handle_edit_block(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let raw_path = args
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'path'".to_string())?;
    let old_text = args
        .get("old_text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'old_text'".to_string())?;
    let new_text = args
        .get("new_text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'new_text'".to_string())?;

    if old_text.is_empty() {
        return Err("Parameter 'old_text' must not be empty".to_string());
    }

    let target = resolve_path(&ctx.workspace_cwd, raw_path);
    let lock = file_lock_for(&target);
    let _permit = lock.lock().await;

    let res = async {
        let original = tokio::fs::read_to_string(&target)
            .await
            .map_err(|e| format!("Failed to read '{}': {e}", target.display()))?;

        let match_count = original.matches(old_text).count();
        if match_count == 0 {
            return Err(format!("old_text not found in '{}'", target.display()));
        }
        if match_count > 1 {
            return Err(format!(
                "old_text matched {match_count} locations in '{}'; must be unique",
                target.display()
            ));
        }

        let updated = original.replacen(old_text, new_text, 1);
        atomic_write(&target, updated.as_bytes()).await?;
        Ok(format!("Successfully edited {}", target.display()))
    }
    .await;

    release_file_lock(&target, &lock);
    res
}

async fn handle_bash(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'command'".to_string())?;

    let cwd = args
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(|raw| resolve_path(&ctx.workspace_cwd, raw))
        .unwrap_or_else(|| ctx.workspace_cwd.clone());

    let timeout = args
        .get("timeout_ms")
        .and_then(|v| v.as_u64())
        .map(Duration::from_millis)
        .unwrap_or(ctx.timeout);

    let call_ctx = ExecutionContext {
        session_id: ctx.session_id.clone(),
        workspace_cwd: cwd,
        timeout,
    };

    match execute_shell_command(command, &call_ctx, DEFAULT_MAX_OUTPUT_BYTES).await {
        Ok(output) => Ok(output),
        Err(ToolExecutionError::NonZeroExit { code, stderr }) => {
            Err(format!("Command exited with code {code}:\n{stderr}"))
        }
        Err(ToolExecutionError::Timeout) => Err(format!(
            "Command timed out after {} ms and process group was killed",
            timeout.as_millis()
        )),
        Err(other) => Err(other.to_string()),
    }
}

async fn handle_grep(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'pattern'".to_string())?;
    if pattern.is_empty() {
        return Err("Pattern must not be empty".to_string());
    }

    let root_raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let root = resolve_path(&ctx.workspace_cwd, root_raw);
    if !root.exists() {
        return Err(format!("Path not found: {}", root.display()));
    }
    let root = std::fs::canonicalize(&root).unwrap_or(root);

    let glob_set = match args.get("glob").and_then(|v| v.as_str()) {
        Some(g) => build_glob_set(g)?,
        None => None,
    };
    let ignore_case = args
        .get("ignoreCase")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let literal = args
        .get("literal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let context_lines = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let match_limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).max(1))
        .unwrap_or(DEFAULT_MATCH_LIMIT);

    let effective_pattern = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(ignore_case)
        .build(&effective_pattern)
        .map_err(|e| format!("Invalid pattern '{pattern}': {e}"))?;
    let matcher = Arc::new(matcher);

    let shared = Arc::new(SearchShared {
        lines: Mutex::new(Vec::new()),
        match_count: AtomicUsize::new(0),
        total_bytes: AtomicUsize::new(0),
        truncated: AtomicBool::new(false),
        match_limit,
    });

    let is_single_file = root.is_file();
    let (mut output, truncated) = tokio::task::spawn_blocking(move || {
        if is_single_file {
            let mut searcher = new_searcher_with_context(context_lines, context_lines);
            let display = root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| root.display().to_string());
            let sink = FileSink {
                shared: &shared,
                display_path: display,
                last_line: None,
                use_separators: context_lines > 0,
            };
            let _ = searcher.search_path(&*matcher, &root, sink);
        } else {
            let walker = build_walker(&root).build_parallel();
            walker.run(|| {
                let matcher = matcher.clone();
                let shared = shared.clone();
                let glob_set = glob_set.clone();
                let root = root.clone();
                Box::new(move |entry| {
                    if shared.at_capacity() {
                        shared.truncated.store(true, Ordering::Relaxed);
                        return WalkState::Quit;
                    }
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    if let Some(gs) = &glob_set {
                        if !gs.is_match(entry.path()) {
                            return WalkState::Continue;
                        }
                    }
                    let mut searcher = new_searcher_with_context(context_lines, context_lines);
                    let display = relative_display(entry.path(), &root);
                    let sink = FileSink {
                        shared: &shared,
                        display_path: display,
                        last_line: None,
                        use_separators: context_lines > 0,
                    };
                    let _ = searcher.search_path(&*matcher, entry.path(), sink);
                    WalkState::Continue
                })
            });
        }
        let truncated = shared.truncated.load(Ordering::Relaxed)
            || shared.match_count.load(Ordering::Relaxed) > shared.match_limit;
        let lines = shared.lines.lock().join("\n");
        (lines, truncated)
    })
    .await
    .map_err(|e| format!("Search task failed: {e}"))?;

    if output.is_empty() {
        return Ok(format!("No matches found for pattern '{pattern}'"));
    }
    if truncated {
        output
            .push_str("\n\n[Output truncated — narrow the pattern/path or pass a smaller 'limit']");
    }
    Ok(output)
}

async fn handle_find(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'pattern'".to_string())?;
    if pattern.trim().is_empty() {
        return Err("Pattern must not be empty".to_string());
    }

    let path_raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let root = resolve_path(&ctx.workspace_cwd, path_raw);
    if !root.exists() {
        return Err(format!("Path not found: {}", root.display()));
    }
    if root.is_file() {
        return Ok(root.display().to_string());
    }

    let glob_set =
        build_glob_set(pattern)?.ok_or_else(|| "Pattern must not be empty".to_string())?;
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

    let (mut output, truncated) = tokio::task::spawn_blocking(move || {
        let walker = build_walker(&root).build_parallel();
        walker.run(|| {
            let shared = shared.clone();
            let glob_set = glob_set.clone();
            let root = root.clone();
            Box::new(move |entry| {
                if shared.at_capacity() {
                    shared.truncated.store(true, Ordering::Relaxed);
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
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
    .map_err(|e| format!("Find task failed: {e}"))?;

    if output.is_empty() {
        return Ok(format!("No files found matching '{pattern}'"));
    }
    if truncated {
        output.push_str("\n\n[Output truncated — narrow the pattern or pass a smaller 'limit']");
    }
    Ok(output)
}

async fn handle_ls(args: &Value, ctx: &ExecutionContext) -> Result<String, String> {
    let path_raw = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let root = resolve_path(&ctx.workspace_cwd, path_raw);
    if !root.exists() {
        return Err(format!("Path not found: {}", root.display()));
    }
    if !root.is_dir() {
        return Err(format!("Not a directory: {}", root.display()));
    }

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<(String, u64)> = Vec::new();
    let mut entries = std::fs::read_dir(&root)
        .map_err(|e| format!("Failed to read directory '{}': {e}", root.display()))?;

    let mut truncated = false;
    while let Some(entry) = entries
        .next()
        .transpose()
        .map_err(|e| format!("Failed to read directory '{}': {e}", root.display()))?
    {
        if dirs.len() + files.len() >= DEFAULT_LS_LIMIT {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            dirs.push(format!("{name}/"));
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
        output.push_str(&format!("{name} ({size} bytes)\n"));
    }
    if output.is_empty() {
        return Ok(format!("Directory is empty: {}", root.display()));
    }
    if truncated {
        output.push_str("\n[Output truncated at 1000 entries]");
    }
    Ok(output.trim_end_matches('\n').to_string())
}

async fn atomic_write(target: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("Failed to create directory '{}': {e}", parent.display()))?;

    let original_permissions = if target.exists() {
        tokio::fs::metadata(target)
            .await
            .ok()
            .map(|m| m.permissions())
    } else {
        None
    };

    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let tmp_path = parent.join(format!(".{file_name}.thunder_tmp_{pid}_{seq}"));
    let mut guard = TempFileGuard::new(tmp_path.clone());

    {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)
            .await
            .map_err(|e| {
                format!(
                    "Failed to create temporary file '{}': {e}",
                    tmp_path.display()
                )
            })?;
        file.write_all(bytes)
            .await
            .map_err(|e| format!("Failed writing to temporary file: {e}"))?;
        file.sync_all()
            .await
            .map_err(|e| format!("Failed syncing temporary file: {e}"))?;
    }

    if let Some(perms) = original_permissions {
        let _ = tokio::fs::set_permissions(&tmp_path, perms).await;
    }

    tokio::fs::rename(&tmp_path, target).await.map_err(|e| {
        format!(
            "Failed to atomically rename '{}' to '{}': {e}",
            tmp_path.display(),
            target.display()
        )
    })?;

    guard.commit();
    Ok(())
}

struct SearchShared {
    lines: Mutex<Vec<String>>,
    match_count: AtomicUsize,
    total_bytes: AtomicUsize,
    truncated: AtomicBool,
    match_limit: usize,
}

impl SearchShared {
    fn at_capacity(&self) -> bool {
        self.match_count.load(Ordering::Relaxed) >= self.match_limit
            || self.total_bytes.load(Ordering::Relaxed) >= MAX_SEARCH_OUTPUT_BYTES
    }

    fn push(&self, line: String) {
        let mut lines = self.lines.lock();
        let bytes = line.len() + 1;
        lines.push(line);
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.total_bytes.load(Ordering::Relaxed) >= MAX_SEARCH_OUTPUT_BYTES {
            self.truncated.store(true, Ordering::Relaxed);
        }
    }
}

struct FileSink<'a> {
    shared: &'a SearchShared,
    display_path: String,
    last_line: Option<u64>,
    use_separators: bool,
}

impl FileSink<'_> {
    fn render(
        &mut self,
        line_number: u64,
        text: &[u8],
        is_match: bool,
    ) -> Result<bool, std::io::Error> {
        if self.use_separators {
            if let Some(prev) = self.last_line {
                if line_number > prev + 1 {
                    self.shared.push("--".to_string());
                }
            }
        }
        self.last_line = Some(line_number);

        let raw = String::from_utf8_lossy(text);
        let trimmed = raw.trim_end_matches(['\n', '\r']);
        let text: String = if trimmed.chars().count() > MAX_LINE_LENGTH {
            let suffix = "… (line truncated)";
            let keep = MAX_LINE_LENGTH.saturating_sub(suffix.chars().count());
            let mut s: String = trimmed.chars().take(keep).collect();
            s.push_str(suffix);
            s
        } else {
            trimmed.to_string()
        };

        let sep = if is_match { ":" } else { "-" };
        self.shared.push(format!(
            "{}{}{}:{}",
            self.display_path, sep, line_number, text
        ));
        Ok(!self.shared.at_capacity())
    }
}

impl Sink for FileSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let count = self.shared.match_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.shared.match_limit {
            self.shared.truncated.store(true, Ordering::Relaxed);
            return Ok(false);
        }
        let start_line = mat.line_number().unwrap_or(0);
        let mut keep_going = true;
        for (i, line) in mat.lines().enumerate() {
            keep_going = self.render(start_line + i as u64, line, true)?;
        }
        Ok(keep_going)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if !matches!(ctx.kind(), SinkContextKind::Before | SinkContextKind::After) {
            return Ok(true);
        }
        let start_line = ctx.line_number().unwrap_or(0);
        let mut keep_going = true;
        for (i, line) in ctx.bytes().split(|&b| b == b'\n').enumerate() {
            if !line.is_empty() {
                keep_going = self.render(start_line + i as u64, line, false)?;
            }
        }
        Ok(keep_going)
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
            || self.total_bytes.load(Ordering::Relaxed) >= MAX_SEARCH_OUTPUT_BYTES
    }
}

fn build_walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder.hidden(false);
    builder.filter_entry(|entry| entry.file_name() != ".git");
    builder.require_git(false);
    builder
}

fn relative_display(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn build_glob_set(glob: &str) -> Result<Option<globset::GlobSet>, String> {
    let pattern = glob.trim();
    if pattern.is_empty() {
        return Ok(None);
    }
    let effective = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    let glob =
        globset::Glob::new(&effective).map_err(|e| format!("Invalid glob '{pattern}': {e}"))?;
    let mut builder = globset::GlobSetBuilder::new();
    builder.add(glob);
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("Invalid glob '{pattern}': {e}"))
}

fn new_searcher_with_context(before: usize, after: usize) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .before_context(before)
        .after_context(after)
        .build()
}
