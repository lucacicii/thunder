use crate::error::SkillError;
use crate::parser::SkillParser;
use crate::types::Skill;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

pub struct SkillLoader;

impl SkillLoader {
    /// Load a single skill from a file path.
    pub async fn load_file(path: impl AsRef<Path>) -> Result<Skill, SkillError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(SkillError::NotFound(format!(
                "Skill file not found: {}",
                p.display()
            )));
        }

        let content = tokio::fs::read_to_string(p).await.map_err(|e| {
            SkillError::IoError(format!("Failed to read skill file {}: {e}", p.display()))
        })?;

        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "json" => SkillParser::parse_json(&content, Some(p)),
            "yaml" | "yml" => SkillParser::parse_yaml(&content, Some(p)),
            _ => SkillParser::parse_markdown(&content, Some(p)),
        }
    }

    /// Recursively load all skills located inside a directory.
    pub async fn load_dir(dir: impl AsRef<Path>) -> Result<Vec<Skill>, SkillError> {
        let dir_path = dir.as_ref();
        if !dir_path.exists() {
            return Err(SkillError::NotFound(format!(
                "Skill directory not found: {}",
                dir_path.display()
            )));
        }

        let mut skills = Vec::new();
        let mut stack = vec![dir_path.to_path_buf()];

        while let Some(current_dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&current_dir).await {
                Ok(rd) => rd,
                Err(e) => {
                    warn!("Failed to read directory {}: {e}", current_dir.display());
                    continue;
                }
            };

            while let Ok(Some(entry)) = entries.next_entry().await {
                let entry_path = entry.path();
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();

                let is_dir = if let Ok(meta) = tokio::fs::metadata(&entry_path).await {
                    meta.is_dir()
                } else {
                    false
                };

                if is_dir {
                    // Ignore .git, target, or top-level node_modules inside subtrees
                    if name_str != ".git" && name_str != "target" {
                        stack.push(entry_path);
                    }
                } else if entry_path.is_file() {
                    let ext = entry_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if ext == "md"
                        || ext == "markdown"
                        || ext == "json"
                        || ext == "yaml"
                        || ext == "yml"
                    {
                        match Self::load_file(&entry_path).await {
                            Ok(skill) => {
                                debug!(skill_name = %skill.name, path = %entry_path.display(), "Loaded skill");
                                skills.push(skill);
                            }
                            Err(e) => {
                                warn!(path = %entry_path.display(), "Failed to parse skill file: {e}");
                            }
                        }
                    }
                }
            }
        }

        info!(dir = %dir_path.display(), count = skills.len(), "Loaded skills from directory");
        Ok(skills)
    }

    /// Load skills from multiple search paths.
    pub async fn load_search_paths(paths: &[PathBuf]) -> Vec<Skill> {
        let mut all_skills = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        for path in paths {
            if path.is_file() {
                if let Ok(skill) = Self::load_file(path).await {
                    if seen_names.insert(skill.name.clone()) {
                        all_skills.push(skill);
                    }
                }
            } else if path.is_dir() {
                if let Ok(skills) = Self::load_dir(path).await {
                    for skill in skills {
                        if seen_names.insert(skill.name.clone()) {
                            all_skills.push(skill);
                        }
                    }
                }
            }
        }

        all_skills
    }

    /// Default system-wide and local search directories for Agent skills.
    pub fn default_search_paths() -> Vec<PathBuf> {
        Self::default_search_paths_from(
            &thunder_agent_loop::core::paths::thunder_config_dir(),
            thunder_agent_loop::core::paths::real_home_dir().as_deref(),
            global_skills_disabled(),
        )
    }

    /// [`default_search_paths`] with both roots — and the global-skills flag —
    /// injected, so precedence and the root split are testable without touching
    /// the process environment.
    ///
    /// `.thunder/skills` is thunder's own and follows its data root; the
    /// `.agents` / `.pi` paths are the *user's* cross-tool surfaces and always
    /// resolve against the real home.
    pub fn default_search_paths_from(
        thunder_home: &Path,
        real_home: Option<&Path>,
        global_disabled: bool,
    ) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // 1. Current workspace relative skill paths
        paths.push(PathBuf::from(".agents/skills"));
        paths.push(PathBuf::from(".pi/skills"));
        paths.push(PathBuf::from("skills"));

        // 2. User-level skill paths (Agent & Thunder standards). "Global" here
        // means user-level, thunder's own included, so the switch also drops
        // `<root>/skills`. Order mirrors the pre-contract list (later wins):
        // .agents, .thunder, .pi/agent, .pi.
        if !global_disabled {
            if let Some(home) = real_home {
                paths.push(home.join(".agents/skills"));
            }
            paths.push(thunder_home.join("skills"));
            if let Some(home) = real_home {
                paths.push(home.join(".pi/agent/skills"));
                paths.push(home.join(".pi/skills"));
            }
        }

        paths
    }

    /// Scan all default search paths and return both the discovered skills and a formatted report string.
    pub async fn scan_default_and_report() -> (Vec<Skill>, String) {
        let paths = Self::default_search_paths();
        let loaded = Self::load_search_paths(&paths).await;
        let mut report = format!(
            "✔ Scanned and indexed **{}** skill(s) across default search paths.\n\n",
            loaded.len()
        );
        for s in &loaded {
            let brief = s.description.lines().next().unwrap_or("").trim();
            report.push_str(&format!("- **`{}`**: {}\n", s.name, brief));
        }
        (loaded, report)
    }
}

/// `THUNDER_SKILLS_NO_GLOBAL=1` — drop user-level skill directories so a run does
/// not inherit cross-workspace skills it never asked for.
fn global_skills_disabled() -> bool {
    std::env::var("THUNDER_SKILLS_NO_GLOBAL")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// thunder's own skills follow its data root; the user's cross-tool skill
    /// directories stay under the real home.
    #[test]
    fn global_skill_paths_split_between_root_and_user_home() {
        let paths = SkillLoader::default_search_paths_from(
            Path::new("/data/thunder"),
            Some(Path::new("/home/u")),
            false,
        );

        assert!(paths.contains(&PathBuf::from("/data/thunder/skills")));
        assert!(paths.contains(&PathBuf::from("/home/u/.agents/skills")));
        assert!(paths.contains(&PathBuf::from("/home/u/.pi/agent/skills")));
        assert!(paths.contains(&PathBuf::from("/home/u/.pi/skills")));
        assert!(
            !paths.iter().any(|p| p.starts_with("/home/u/.thunder")),
            "thunder's own skills must come from the data root, not the user home: {paths:?}"
        );
    }

    /// Order is load-bearing (later paths win), so it must match the list this
    /// produced before the data-root contract existed.
    #[test]
    fn global_skill_path_order_is_preserved() {
        let paths = SkillLoader::default_search_paths_from(
            Path::new("/data/thunder"),
            Some(Path::new("/home/u")),
            false,
        );
        let globals: Vec<String> = paths
            .iter()
            .filter(|p| p.is_absolute())
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            globals,
            vec![
                "/home/u/.agents/skills",
                "/data/thunder/skills",
                "/home/u/.pi/agent/skills",
                "/home/u/.pi/skills",
            ]
        );
    }

    /// No user home at all: still resolves, and still offers thunder's own dir.
    #[test]
    fn missing_user_home_keeps_thunders_own_skills() {
        let paths = SkillLoader::default_search_paths_from(Path::new("/data/thunder"), None, false);
        assert!(paths.contains(&PathBuf::from("/data/thunder/skills")));
        assert!(paths.contains(&PathBuf::from("skills")));
    }

    /// The global switch drops every user-level path, thunder's own included —
    /// what `THUNDER_SKILLS_NO_GLOBAL` did before the split.
    #[test]
    fn global_switch_drops_all_user_level_paths() {
        let paths = SkillLoader::default_search_paths_from(
            Path::new("/data/thunder"),
            Some(Path::new("/home/u")),
            true,
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from(".agents/skills"),
                PathBuf::from(".pi/skills"),
                PathBuf::from("skills"),
            ]
        );
    }
}
