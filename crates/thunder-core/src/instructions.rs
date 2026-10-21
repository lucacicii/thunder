use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

pub const CANDIDATE_FILENAMES: &[&str] = &[
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

fn strip_bom(content: &str) -> &str {
    content.strip_prefix('\u{feff}').unwrap_or(content)
}

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

pub fn is_global_instructions_disabled() -> bool {
    std::env::var("THUNDER_NO_GLOBAL_INSTRUCTIONS")
        .or_else(|_| std::env::var("THUNDER_SKILLS_NO_GLOBAL"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextInstructionFile {
    pub path: PathBuf,
    pub content: String,
}

/// Discover hierarchical `AGENTS.md` / `CLAUDE.md` files from global user dirs and outermost ancestor down to `workspace_root`.
pub async fn discover_context_files(
    workspace_root: Option<&Path>,
    home_dir: Option<&Path>,
    thunder_root: Option<&Path>,
) -> Vec<ContextInstructionFile> {
    let mut results = Vec::new();
    let mut seen_canonical_paths = HashSet::new();

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
                let canonical = file_path
                    .canonicalize()
                    .unwrap_or_else(|_| file_path.clone());
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

    if let Some(ws) = workspace_root {
        let mut ancestor_files = Vec::new();
        let mut curr = Some(ws.to_path_buf());

        while let Some(dir) = curr {
            if let Some(file_path) = find_context_file_in_dir(&dir) {
                let canonical = file_path
                    .canonicalize()
                    .unwrap_or_else(|_| file_path.clone());
                if seen_canonical_paths.insert(canonical) {
                    match tokio::fs::read_to_string(&file_path).await {
                        Ok(raw) => {
                            let content = strip_bom(&raw).trim().to_string();
                            if !content.is_empty() {
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

        ancestor_files.reverse();
        results.extend(ancestor_files);
    }

    results
}

/// Format discovered instruction files into a system prompt block.
pub fn format_instructions_section(files: &[ContextInstructionFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let mut out = String::from("<project_context>\n");
    for file in files {
        out.push_str(&format!(
            "<project_instructions path=\"{}\">\n{}\n</project_instructions>\n",
            file.path.display(),
            file.content
        ));
    }
    out.push_str("</project_context>");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn discovers_ancestor_to_leaf_instructions_in_order() {
        let dir = tempdir().unwrap();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        tokio::fs::create_dir_all(&child).await.unwrap();

        tokio::fs::write(parent.join("AGENTS.md"), "parent rules")
            .await
            .unwrap();
        tokio::fs::write(child.join("CLAUDE.md"), "child rules")
            .await
            .unwrap();

        let files = discover_context_files(Some(&child), None, None).await;
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].content, "parent rules");
        assert_eq!(files[1].content, "child rules");
    }
}
