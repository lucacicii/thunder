//! Long-term project / user memory, injected into the system prompt.
//!
//! Thunder keeps a small set of hand-written markdown notes that should follow
//! a project across sessions — build quirks, conventions, "we do not do X
//! here". They are gathered from a layered set of files and appended to the
//! base system prompt, so the model starts every run already knowing them.
//!
//! Sources, lowest precedence first:
//!
//! 1. `~/.thunder/THUNDER.md`            — user-global notes
//! 2. `<ws>/.thunder/THUNDER.md`         — project notes (committed)
//! 3. `<ws>/.thunder/memory/*.md`        — project notes, split into topics
//! 4. `<ws>/.thunder/THUNDER.local.md`   — machine-local notes (git-ignored)
//! 5. `<ws>/.thunder/<agent.memory.files[]>` — extra files named in config.json
//!
//! Any file may pull in another with an `@path/to/file.md` line. Imports are
//! resolved relative to the importing file, jailed to the workspace root and
//! the thunder home, and bounded by depth (see [`MAX_IMPORT_DEPTH`]) and total
//! size so a stray `@` cannot read the filesystem or loop forever.
//!
//! ## Prompt-cache stability
//!
//! The memory block sits at position 0 of every request, exactly the region a
//! provider's prompt cache keys on. Re-reading files on every turn would
//! invalidate that cache whenever a file's mtime changed. The plugin reads the
//! files **once per init** and caches the rendered block, so a single host
//! instance keeps a stable prefix for its lifetime; an edit is picked up the
//! next time the host rebuilds the run.

use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use crate::thunder_config::{ThunderConfig, THUNDER_DIR};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Conventional memory file name, at the user and project scope.
pub const MEMORY_FILE: &str = "THUNDER.md";
/// Machine-local (git-ignored) memory file name.
pub const MEMORY_LOCAL_FILE: &str = "THUNDER.local.md";
/// Sub-directory holding topic-split memory notes.
pub const MEMORY_SUBDIR: &str = "memory";

/// Maximum `@import` nesting depth.
pub const MAX_IMPORT_DEPTH: usize = 5;
/// Maximum total bytes of memory rendered into the prompt.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;

/// A plugin that appends layered `.thunder` memory to the system prompt.
pub struct MemoryPlugin {
    manifest: PluginManifest,
    /// Rendered memory block. `None` before the first init, or when there is
    /// nothing to inject. Shared via `Arc` so clones observe one cache.
    cache: Arc<RwLock<Option<String>>>,
}

/// Mutable accumulator threaded through the recursive file walk.
struct RenderState {
    out: String,
    budget: usize,
    roots: Vec<PathBuf>,
    /// Files already emitted anywhere in this render (dedupe across sources).
    seen: HashSet<PathBuf>,
}

impl MemoryPlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            "memory",
            "Thunder Long-Term Memory",
            "Loads layered THUNDER.md / memory notes from the user and project scopes and injects them into the system prompt.",
            "0.1.0",
        )
        .with_capability(PluginCapability::MemoryPersistence)
        // Baseline plugin: stays active so the prompt block is stable across
        // turns, whether or not any memory file currently exists.
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Render the memory block for `workspace`, honouring `agent.memory`.
    /// Returns `None` when disabled or when no source file exists.
    async fn render(workspace: Option<&Path>) -> Option<String> {
        let cfg = ThunderConfig::load(workspace).await;
        let mem = cfg.memory();
        if !mem.enabled {
            return None;
        }

        let ws = workspace?;
        let thunder_dir = ws.join(THUNDER_DIR);

        // Ordered sources. Later ones read as "more specific", matching how a
        // reader expects overrides to appear.
        let mut sources: Vec<PathBuf> = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            sources.push(PathBuf::from(home).join(THUNDER_DIR).join(MEMORY_FILE));
        }
        sources.push(thunder_dir.join(MEMORY_FILE));

        // Topic-split notes, deterministic order.
        let subdir = thunder_dir.join(MEMORY_SUBDIR);
        if let Ok(mut entries) = tokio::fs::read_dir(&subdir).await {
            let mut files: Vec<PathBuf> = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("md") {
                    files.push(p);
                }
            }
            files.sort();
            sources.append(&mut files);
        }

        sources.push(thunder_dir.join(MEMORY_LOCAL_FILE));

        // Extra files named in config.json, resolved under `<ws>/.thunder/`.
        if let Some(extra) = &mem.files {
            for rel in extra {
                sources.push(thunder_dir.join(rel));
            }
        }

        let mut state = RenderState {
            out: String::new(),
            budget: MAX_TOTAL_BYTES,
            roots: Self::allowed_roots(ws),
            seen: HashSet::new(),
        };

        for src in sources {
            // A fresh `visited` per top-level source: cycle detection is only
            // meaningful within one import chain, while `seen` dedupes globally.
            let mut visited = HashSet::new();
            Self::append_file(src, 0, &mut state, &mut visited).await;
        }

        let trimmed = state.out.trim();
        if trimmed.is_empty() {
            return None;
        }

        Some(format!(
            "The following notes describe this project and the user's standing \
             preferences. Treat them as established context; follow them unless \
             the user's current request says otherwise.\n\n{trimmed}"
        ))
    }

    /// Roots an import may resolve inside: the workspace and the thunder home.
    fn allowed_roots(ws: &Path) -> Vec<PathBuf> {
        let mut roots = vec![ws.canonicalize().unwrap_or_else(|_| ws.to_path_buf())];
        if let Some(home) = std::env::var_os("HOME") {
            let h = PathBuf::from(home).join(THUNDER_DIR);
            roots.push(h.canonicalize().unwrap_or(h));
        }
        roots
    }

    fn is_allowed(path: &Path, roots: &[PathBuf]) -> bool {
        // A non-existent path cannot be read; reject it so the jail check never
        // runs on an unresolved path.
        match path.canonicalize() {
            Ok(canonical) => roots.iter().any(|root| canonical.starts_with(root)),
            Err(_) => false,
        }
    }

    fn append_file<'a>(
        path: PathBuf,
        depth: usize,
        state: &'a mut RenderState,
        visited: &'a mut HashSet<PathBuf>,
    ) -> Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if depth > MAX_IMPORT_DEPTH {
                tracing::warn!(path = %path.display(), "Memory import nesting exceeded; skipping");
                return;
            }
            if !Self::is_allowed(&path, &state.roots) {
                return;
            }
            let canonical = match path.canonicalize() {
                Ok(c) => c,
                Err(_) => return,
            };
            // Reject cycles within one chain, and files already emitted anywhere.
            if !visited.insert(canonical.clone()) || !state.seen.insert(canonical.clone()) {
                return;
            }

            let Ok(raw) = tokio::fs::read_to_string(&canonical).await else {
                return;
            };
            if state.budget == 0 {
                return;
            }

            let display = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("memory");

            // Emit the file body, collecting imports to expand afterwards so a
            // file's own notes appear before its imports.
            let mut body = String::new();
            let mut imports: Vec<PathBuf> = Vec::new();
            for line in raw.lines() {
                if let Some(target) = line.strip_prefix('@') {
                    let target = target.trim();
                    if !target.is_empty() {
                        let resolved = if Path::new(target).is_absolute() {
                            PathBuf::from(target)
                        } else {
                            canonical
                                .parent()
                                .map(|p| p.join(target))
                                .unwrap_or_else(|| PathBuf::from(target))
                        };
                        imports.push(resolved);
                        continue; // do not echo the directive into the prompt
                    }
                }
                body.push_str(line);
                body.push('\n');
            }

            let mut chunk = format!("\n\n#### {display}\n");
            chunk.push_str(body.trim_end());
            let end = Self::floor_char_boundary(&chunk, chunk.len().min(state.budget));
            state.out.push_str(&chunk[..end]);
            state.budget -= end;

            for imp in imports {
                Self::append_file(imp, depth + 1, state, visited).await;
            }
        })
    }

    /// Largest index `<= i` that is a UTF-8 char boundary.
    fn floor_char_boundary(s: &str, i: usize) -> usize {
        if i >= s.len() {
            return s.len();
        }
        let mut idx = i;
        while idx > 0 && !s.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }
}

impl Default for MemoryPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ThunderPlugin for MemoryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        // Sync by contract: read whatever `on_init` staged. `try_read` keeps a
        // contended lock from blocking prompt assembly; a miss simply yields no
        // contribution for this run.
        self.cache.try_read().ok()?.clone()
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let rendered = Self::render(ctx.workspace_dir.as_deref()).await;
        *self.cache.write().await = rendered;
        Ok(())
    }
}
