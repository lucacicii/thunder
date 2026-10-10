use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// Candidate context instruction filenames, ordered by priority (aligning with Pi).
pub const CANDIDATE_FILENAMES: &[&str] = &[
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

/// Remove UTF-8 Byte Order Mark (BOM) if present.
fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

/// Find the highest priority context file in a directory, if any exists.
pub fn find_context_file_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    for filename in CANDIDATE_FILENAMES {
        let file_path = dir.join(filename);
        if file_path.is_file() {
            return Some(file_path);
        }
    }
    None
}

/// Check if global instruction file scanning is disabled via environment variable.
pub fn is_global_instructions_disabled() -> bool {
    std::env::var("THUNDER_NO_GLOBAL_INSTRUCTIONS")
        .or_else(|_| std::env::var("THUNDER_SKILLS_NO_GLOBAL"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Discovered context instruction file with its path and content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInstructionFile {
    pub path: PathBuf,
    pub content: String,
}

/// Discover context instruction files (e.g. AGENTS.md) adhering to Pi's resolution rules:
/// 1. Global user directories (`~/.agents`, `~/.pi/agent`, `~/.thunder`)
/// 2. Ancestor hierarchy of the workspace from outermost ancestor down to `workspace_root`
///
/// `home_dir` is the *real* user home (`.agents`, `.pi/agent` are the user's own
/// cross-tool surfaces); `thunder_root` is thunder's data root
/// ([`thunder_agent_loop::core::paths`]). They are separate arguments so an
/// embedding host can relocate thunder's state without disturbing the user's.
pub async fn discover_context_files(
    workspace_root: Option<&Path>,
    home_dir: Option<&Path>,
    thunder_root: Option<&Path>,
) -> Vec<ContextInstructionFile> {
    let mut results = Vec::new();
    let mut seen_canonical_paths = HashSet::new();

    // 1. Scan global user directories if not disabled
    if !is_global_instructions_disabled() {
        let mut global_candidates: Vec<PathBuf> = Vec::new();
        if let Some(home) = home_dir {
            global_candidates.push(home.join(".agents"));
            global_candidates.push(home.join(".pi").join("agent"));
        }
        if let Some(thunder_home) = thunder_root {
            global_candidates.push(thunder_home.to_path_buf());
        }
        for candidate_dir in global_candidates {
            if let Some(file_path) = find_context_file_in_dir(&candidate_dir) {
                let canonical = file_path.canonicalize().unwrap_or_else(|_| file_path.clone());
                if seen_canonical_paths.insert(canonical) {
                    match tokio::fs::read_to_string(&file_path).await {
                        Ok(raw) => {
                            let content = strip_bom(&raw).trim().to_string();
                            if !content.is_empty() {
                                debug!(path = %file_path.display(), "Loaded global context instruction file");
                                results.push(ContextInstructionFile {
                                    path: file_path,
                                    content,
                                });
                                // Match Pi: load the first matched global context file
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(path = %file_path.display(), error = %e, "Failed to read global context file");
                        }
                    }
                }
            }
        }
    }

    // 2. Scan workspace ancestor chain: from outermost root to leaf workspace
    if let Some(ws) = workspace_root {
        let mut ancestor_files = Vec::new();
        let mut curr = Some(ws.to_path_buf());

        while let Some(dir) = curr {
            if let Some(file_path) = find_context_file_in_dir(&dir) {
                let canonical = file_path.canonicalize().unwrap_or_else(|_| file_path.clone());
                if seen_canonical_paths.insert(canonical) {
                    match tokio::fs::read_to_string(&file_path).await {
                        Ok(raw) => {
                            let content = strip_bom(&raw).trim().to_string();
                            if !content.is_empty() {
                                debug!(path = %file_path.display(), "Discovered workspace context instruction file");
                                // unshift / prepend: outermost ancestor files appear before leaf workspace
                                ancestor_files.push(ContextInstructionFile {
                                    path: file_path,
                                    content,
                                });
                            }
                        }
                        Err(e) => {
                            warn!(path = %file_path.display(), error = %e, "Failed to read workspace context file");
                        }
                    }
                }
            }

            curr = dir.parent().map(Path::to_path_buf);
        }

        // We walked bottom-up (workspace -> root), reverse so outer ancestors come first
        ancestor_files.reverse();
        results.extend(ancestor_files);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_candidate_precedence() {
        let dir = tempdir().unwrap();
        let p = dir.path();

        tokio::fs::write(p.join("CLAUDE.md"), "claude content").await.unwrap();
        assert_eq!(
            find_context_file_in_dir(p),
            Some(p.join("CLAUDE.md"))
        );

        tokio::fs::write(p.join("AGENTS.md"), "agents content").await.unwrap();
        assert_eq!(
            find_context_file_in_dir(p),
            Some(p.join("AGENTS.md"))
        );

        tokio::fs::write(p.join("AGENTS.override.md"), "override content").await.unwrap();
        assert_eq!(
            find_context_file_in_dir(p),
            Some(p.join("AGENTS.override.md"))
        );
    }

    #[tokio::test]
    async fn test_ancestor_order_outermost_to_innermost() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        tokio::fs::create_dir_all(&child).await.unwrap();

        tokio::fs::write(parent.join("AGENTS.md"), "parent instructions").await.unwrap();
        tokio::fs::write(child.join("AGENTS.md"), "child instructions").await.unwrap();

        let files = discover_context_files(Some(&child), None, None).await;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, parent.join("AGENTS.md"));
        assert_eq!(files[0].content, "parent instructions");
        assert_eq!(files[1].path, child.join("AGENTS.md"));
        assert_eq!(files[1].content, "child instructions");
    }

    #[tokio::test]
    async fn test_global_context_loaded() {
        let home = tempdir().unwrap();
        let agents_dir = home.path().join(".agents");
        tokio::fs::create_dir_all(&agents_dir).await.unwrap();
        tokio::fs::write(agents_dir.join("AGENTS.md"), "global agents").await.unwrap();

        let ws = tempdir().unwrap();
        tokio::fs::write(ws.path().join("AGENTS.md"), "ws agents").await.unwrap();

        let files = discover_context_files(Some(ws.path()), Some(home.path()), None).await;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, agents_dir.join("AGENTS.md"));
        assert_eq!(files[0].content, "global agents");
        assert_eq!(files[1].path, ws.path().join("AGENTS.md"));
        assert_eq!(files[1].content, "ws agents");
    }

    /// `.agents` / `.pi` resolve against the real home; thunder's own global file
    /// resolves through its data root, passed separately so an embedding host can
    /// relocate thunder's state without moving the user's cross-tool files.
    #[tokio::test]
    async fn test_thunder_root_is_scanned_separately_from_user_home() {
        let home = tempdir().unwrap();
        let thunder_root = tempdir().unwrap();
        let user_agents = home.path().join(".agents");
        tokio::fs::create_dir_all(&user_agents).await.unwrap();
        tokio::fs::write(user_agents.join("AGENTS.md"), "user agents")
            .await
            .unwrap();
        tokio::fs::write(thunder_root.path().join("AGENTS.md"), "thunder global")
            .await
            .unwrap();

        // Pi loads only the first global match, and the user's own dir wins.
        let files =
            discover_context_files(None, Some(home.path()), Some(thunder_root.path())).await;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, user_agents.join("AGENTS.md"));

        // With no user-level file, thunder's root is what gets scanned.
        let empty_home = tempdir().unwrap();
        let files =
            discover_context_files(None, Some(empty_home.path()), Some(thunder_root.path())).await;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, thunder_root.path().join("AGENTS.md"));
        assert_eq!(files[0].content, "thunder global");
    }
}
