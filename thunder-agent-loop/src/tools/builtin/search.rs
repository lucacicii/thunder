//! Fast content search built on ripgrep's core libraries (`grep-searcher` +
//! `grep-regex` + `ignore`). Mirrors the semantics of the `ripgrep` CLI that
//! pi's grep tool spawns, but embedded in-process: no subprocess overhead and
//! no external binary to distribute.

use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use parking_lot::Mutex;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Max matched lines returned (same as pi's DEFAULT_LIMIT for grep).
const DEFAULT_MATCH_LIMIT: usize = 100;
/// Max chars per matched/context line (pi's GREP_MAX_LINE_LENGTH).
const MAX_LINE_LENGTH: usize = 500;
/// Hard cap on collected output before the walk stops (pi's DEFAULT_MAX_BYTES).
const MAX_OUTPUT_BYTES: usize = 50 * 1024;

#[derive(Default)]
pub struct GrepTool {
    default_cwd: Option<PathBuf>,
}

impl GrepTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

/// State shared across the parallel directory walk.
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
            || self.total_bytes.load(Ordering::Relaxed) >= MAX_OUTPUT_BYTES
    }

    fn push(&self, line: String) {
        let mut lines = self.lines.lock();
        let bytes = line.len() + 1;
        lines.push(line);
        self.total_bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.total_bytes.load(Ordering::Relaxed) >= MAX_OUTPUT_BYTES {
            self.truncated.store(true, Ordering::Relaxed);
        }
    }
}

/// Per-file sink rendering `path:line:text` matches and `path-line-text`
/// context lines with `--` separators between discontinuous groups.
struct FileSink<'a> {
    shared: &'a SearchShared,
    display_path: String,
    last_line: Option<u64>,
    /// `--` separators between discontinuous groups (only when context is on).
    use_separators: bool,
}

impl FileSink<'_> {
    fn render(&mut self, line_number: u64, text: &[u8], is_match: bool) -> Result<bool, std::io::Error> {
        // Insert a group separator after any gap (like grep's `--`).
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
        self.shared
            .push(format!("{}{}{}:{}", self.display_path, sep, line_number, text));
        Ok(!self.shared.at_capacity())
    }
}

impl Sink for FileSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
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
        // `Other` contexts (e.g. the first line of a file) are noise for us.
        if !matches!(ctx.kind(), SinkContextKind::Before | SinkContextKind::After) {
            return Ok(true);
        }
        let start_line = ctx.line_number().unwrap_or(0);
        let mut keep_going = true;
        for (i, line) in split_lines(ctx.bytes()).into_iter().enumerate() {
            if !line.is_empty() {
                keep_going = self.render(start_line + i as u64, line, false)?;
            }
        }
        Ok(keep_going)
    }
}

/// Builds the traversal root + walker config shared by GrepTool/FindTool.
pub(crate) fn build_walker(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    // rg defaults: search hidden files but never descend into `.git`.
    builder.hidden(false);
    builder.filter_entry(|entry| entry.file_name() != ".git");
    // Respect .gitignore even outside git repositories (fd's --no-require-git).
    builder.require_git(false);
    builder
}

/// Splits a byte blob (possibly multi-line) into `\n`-separated slices.
fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    bytes.split(|&b| b == b'\n').collect()
}

/// Relativizes `path` against `root` when possible; posix separators.
pub(crate) fn relative_display(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

/// Builds a glob matcher from a user-supplied glob. Globs without a directory
/// component (`*.ts`) are promoted to any-depth (`**/*.ts`) — what callers
/// almost always mean.
pub(crate) fn build_glob_set(glob: &str) -> Result<Option<globset::GlobSet>, String> {
    let pattern = glob.trim();
    if pattern.is_empty() {
        return Ok(None);
    }
    let effective = if pattern.contains('/') {
        pattern.to_string()
    } else {
        format!("**/{}", pattern)
    };
    let glob = globset::Glob::new(&effective)
        .map_err(|e| format!("Invalid glob '{}': {}", pattern, e))?;
    let mut builder = globset::GlobSetBuilder::new();
    builder.add(glob);
    builder
        .build()
        .map(Some)
        .map_err(|e| format!("Invalid glob '{}': {}", pattern, e))
}

#[async_trait]
impl AgentTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "grep",
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (regex or literal string)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"
                    },
                    "ignoreCase": {
                        "type": "boolean",
                        "description": "Case-insensitive search (default: false)"
                    },
                    "literal": {
                        "type": "boolean",
                        "description": "Treat pattern as literal string instead of regex (default: false)"
                    },
                    "context": {
                        "type": "integer",
                        "description": "Number of lines to show before and after each match (default: 0)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of matches to return (default: 100)"
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
        if pattern.is_empty() {
            return Err("Pattern must not be empty".to_string());
        }

        let root_raw = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let root = self
            .default_cwd
            .as_ref()
            .map(|cwd| cwd.join(root_raw))
            .unwrap_or_else(|| PathBuf::from(root_raw));
        if !root.exists() {
            return Err(format!("Path not found: {}", root.display()));
        }
        let root = std::fs::canonicalize(&root).unwrap_or(root);

        let glob_set = match args.get("glob").and_then(|v| v.as_str()) {
            Some(g) => build_glob_set(g)?,
            None => None,
        };
        let ignore_case = args.get("ignoreCase").and_then(|v| v.as_bool()).unwrap_or(false);
        let literal = args.get("literal").and_then(|v| v.as_bool()).unwrap_or(false);
        let context = args.get("context").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
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
            .map_err(|e| format!("Invalid pattern '{}': {}", pattern, e))?;
        let matcher = Arc::new(matcher);

        let shared = Arc::new(SearchShared {
            lines: Mutex::new(Vec::new()),
            match_count: AtomicUsize::new(0),
            total_bytes: AtomicUsize::new(0),
            truncated: AtomicBool::new(false),
            match_limit,
        });

        let cancel = ctx.cancellation_token.clone();
        let is_single_file = root.is_file();
        let context_lines = context;

        // The parallel walk is synchronous; keep it off the async reactor.
        let result = tokio::task::spawn_blocking(move || {
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
        .map_err(|e| format!("Search task failed: {}", e))?;

        let (mut output, truncated) = result;
        if output.is_empty() {
            return Ok(format!("No matches found for pattern '{}'", pattern));
        }
        if truncated {
            output.push_str("\n\n[Output truncated — narrow the pattern/path or pass a smaller 'limit']");
        }
        Ok(output)
    }
}

/// Builds a searcher; `before`/`after` > 0 enable context lines.
pub(crate) fn new_searcher_with_context(before: usize, after: usize) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .before_context(before)
        .after_context(after)
        .build()
}
