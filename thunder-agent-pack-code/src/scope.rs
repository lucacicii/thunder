//! What a call's target actually means, as the code pack sees it.
//!
//! The kernel asks "where does this land"; this file answers. That split is the
//! point of the whole redesign: the *knowledge* of what a workspace is, and which
//! paths matter, belongs here, while the *decision* of what to do about it stays
//! in the kernel's mode policy. Either half can be replaced without the other.
//!
//! Three answers, and only the first two are ever about the working area:
//!
//! * [`Scope::InScope`] — inside a root the host declared. Earns a write the
//!   automatic allowance that `acceptEdits` grants; otherwise changes nothing.
//! * [`Scope::OutOfScope`] — outside every root. **Not a refusal.** The user is
//!   asked, which is the entire difference between this and the jail it replaces.
//! * [`Scope::Critical`] — off limits *without asking*, because asking would be
//!   the wrong shape of question: `.git` (a wrong answer corrupts the repository
//!   irrecoverably), the system and its devices, and the credential store.
//!
//! An unanswerable target is [`Scope::Unknown`], never a guess. `bash` is the
//! standing example: a shell command's targets are computed at runtime, so the
//! honest classification of "rm -rf $BUILD_DIR" is that this file cannot tell —
//! and being asked about a shell command is a far better outcome than being
//! refused for one that a parser misread.

use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_types::scope::{Scope, ToolScope};

/// Argument keys that name a call's target.
const TARGET_KEYS: &[&str] = &["path", "file_path", "filePath", "target", "destination"];

/// Locations the agent may read but never write.
///
/// Deliberately write-only: `cat /etc/hosts` and `grep -r /usr/share/doc` are
/// ordinary work, and the jail being removed here refused exactly those.
const PROTECTED_WRITE_PREFIXES: &[&str] = &[
    "/etc",
    "/private/etc",
    "/dev",
    "/proc",
    "/sys",
    "/boot",
    "/bin",
    "/sbin",
    "/usr",
    "/var/db",
    "/private/var/db",
    "/System",
    "/Library",
    "/Applications",
];

/// Device nodes that are legitimate write targets (`> /dev/null`, `2> /dev/tty`).
const WRITABLE_DEVICES: &[&str] = &[
    "/dev/null",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/tty",
];

/// The code pack's answer to "where does this call land".
#[derive(Debug, Clone)]
pub struct CodeScope {
    workspace_root: PathBuf,
    /// Roots treated as inside the working area (canonicalized at construction).
    roots: Vec<PathBuf>,
    /// Off limits in both directions — the credential store.
    protections: Vec<PathBuf>,
    /// Readable but not writable — thunder's own data root.
    write_blocked: Vec<PathBuf>,
}

impl CodeScope {
    /// The scope a code host installs, derived from the run's configuration.
    ///
    /// Deliberately the same inputs the pipeline derives its roots from, so the
    /// two can never disagree about which directories exist.
    pub fn from_agent_config(
        config: &thunder_agent_loop::types::config::AgentConfig,
        workspace_root: Option<&Path>,
    ) -> Self {
        let root = workspace_root
            .map(Path::to_path_buf)
            .or_else(|| config.workspace_dir.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        Self::new(root)
            .with_extra_roots(config.extra_workspace_roots.clone())
            // Spilled tool output is written by the kernel and read back by the
            // model, so it has to be inside the working area even though it is
            // not inside the project.
            .with_extra_roots([config.scratchpad.base_dir.clone()])
            .with_thunder_data_root()
    }

    /// A scope for `workspace_root`, with the pack's default protections in place.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = canonical(workspace_root.into());
        Self {
            roots: vec![workspace_root.clone()],
            workspace_root,
            protections: Vec::new(),
            write_blocked: Vec::new(),
        }
    }

    /// Treat additional roots (referenced repositories) as inside the workspace.
    pub fn with_extra_roots(mut self, roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        for root in roots {
            self.add_root(root.into());
        }
        self
    }

    /// Add one root, ignoring duplicates and its already-canonical form.
    pub fn add_root(&mut self, root: PathBuf) -> &mut Self {
        let canonical = canonical(root);
        if !self.roots.contains(&canonical) {
            self.roots.push(canonical);
        }
        self
    }

    /// Mark a path off limits in both directions.
    pub fn protect(mut self, path: impl Into<PathBuf>) -> Self {
        self.protections.push(canonical(path.into()));
        self
    }

    /// Protect thunder's own data root, as resolved from the environment.
    ///
    /// The agent has no business rewriting its own credentials, its conversation
    /// store or its config directly — and a `read_file` of `auth.json` would put
    /// every provider key into the model's context.
    pub fn with_thunder_data_root(self) -> Self {
        use thunder_agent_loop::core::paths::thunder_subdir;
        self.protect_data_root(thunder_subdir(""))
    }

    /// Protect `root` as a thunder data root: the credentials inside it are off
    /// limits outright, and everything else there may be read but not written.
    pub fn protect_data_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = canonical(root.into());
        for secret in ["auth.json", "models.json", "credentials.json"] {
            self.protections.push(root.join(secret));
        }
        self.write_blocked.push(root);
        self
    }

    /// The roots this scope considers inside the working area.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Classify one target string.
    ///
    /// Public because the shell-command allowance has to classify individual
    /// operands, not just a tool's `path` argument.
    pub fn classify(&self, target: &str, effect: ToolEffect) -> Scope {
        let Some(candidate) = self.candidate(target) else {
            return Scope::Unknown;
        };
        // Resolve symlinks and `..` the way the kernel would, so a symlink out of
        // the workspace is judged by where it really leads, not by how it is spelt.
        let resolved = normalize_path(&resolve_for_check(&candidate));
        let write = effect != ToolEffect::Read;

        let device = WRITABLE_DEVICES
            .iter()
            .any(|device| resolved == Path::new(device));

        if !device {
            if self.protections.iter().any(|p| resolved.starts_with(p)) {
                return Scope::Critical;
            }
            if write {
                if self.write_blocked.iter().any(|p| resolved.starts_with(p)) {
                    return Scope::Critical;
                }
                if has_git_component(&resolved) {
                    return Scope::Critical;
                }
                if is_protected_workspace_config(&resolved) {
                    return Scope::Critical;
                }
                if PROTECTED_WRITE_PREFIXES
                    .iter()
                    .any(|prefix| resolved.starts_with(prefix))
                {
                    return Scope::Critical;
                }
            }
        }

        if self.roots.iter().any(|root| resolved.starts_with(root)) {
            Scope::InScope
        } else {
            Scope::OutOfScope
        }
    }

    /// The absolute path a target string denotes, or `None` when it cannot be
    /// known (an unset `HOME` behind a `~`).
    fn candidate(&self, target: &str) -> Option<PathBuf> {
        let expanded = expand_home(target)?;
        let path = Path::new(&expanded);
        Some(if path.is_relative() {
            self.workspace_root.join(path)
        } else {
            path.to_path_buf()
        })
    }
}

impl ToolScope for CodeScope {
    fn scope_of(&self, _tool: &str, effect: ToolEffect, arguments: &Value) -> Scope {
        // A shell command's targets are only known once it runs, so a call that
        // executes is never classified — it is asked about, not guessed at.
        if matches!(effect, ToolEffect::Exec | ToolEffect::Other) {
            return Scope::Unknown;
        }
        let Some(target) = TARGET_KEYS
            .iter()
            .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        else {
            return Scope::Unknown;
        };
        self.classify(target, effect)
    }
}

/// Whether any component of `path` is a repository's metadata directory.
fn has_git_component(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::Normal(name) if name == ".git"))
}

/// Whether `path` targets a workspace-level configuration or MCP server
/// declaration file that could alter host execution policy or spawn commands
/// on subsequent turns if modified by the agent.
fn is_protected_workspace_config(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if matches!(file_name, ".mcp.json" | "mcp_servers.json" | ".thunder") {
        return true;
    }
    if let Some(parent_name) = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    {
        if parent_name == ".thunder"
            && matches!(
                file_name,
                "config.json"
                    | "config.local.json"
                    | "mcp.json"
                    | "models.json"
                    | "auth.json"
                    | "credentials.json"
            )
        {
            return true;
        }
    }
    false
}

/// A path made absolute and symlink-free as far as it exists.
fn canonical(path: PathBuf) -> PathBuf {
    normalize_path(&path.canonicalize().unwrap_or(path))
}

/// Resolves a path to its true filesystem location, component by component:
///
///   * every existing prefix is canonicalized (so symlinks — including macOS's
///     `/var → /private/var` — are expanded, and `link/..` follows the link's
///     real parent);
///   * a segment that does not exist cannot be a symlink, so it and any `..`
///     after it are applied lexically (`ws/missing/../../x` is `parent(ws)/x`).
///
/// Expects an absolute path.
fn resolve_for_check(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                out.push(name);
                if let Ok(canonical) = out.canonicalize() {
                    out = canonical;
                }
            }
        }
    }
    out
}

/// Resolves `.` and `..` lexically, without touching the filesystem.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(p) => out.push(Component::Prefix(p)),
            Component::RootDir => out.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

/// Expands a leading `~` / `$HOME` / `${HOME}`. `None` when `HOME` is unknown.
fn expand_home(target: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let rest = if target == "~" {
        ""
    } else if let Some(rest) = target.strip_prefix("~/") {
        return Some(format!("{home}/{rest}"));
    } else if let Some(rest) = target.strip_prefix("${HOME}") {
        rest
    } else if let Some(rest) = target.strip_prefix("$HOME") {
        rest
    } else {
        return Some(target.to_string());
    };
    Some(format!("{home}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope_in(dir: &Path) -> CodeScope {
        CodeScope::new(dir)
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thunder_scope_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_path_inside_the_workspace_is_in_scope() {
        let ws = temp("in");
        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify(&ws.join("src/main.rs").to_string_lossy(), ToolEffect::Write),
            Scope::InScope
        );
        assert_eq!(
            scope.classify("src/main.rs", ToolEffect::Write),
            Scope::InScope,
            "a relative path is resolved against the workspace"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn an_extra_root_is_in_scope_too() {
        let ws = temp("extra_ws");
        let repo = temp("extra_repo");
        let scope = scope_in(&ws).with_extra_roots([repo.clone()]);
        assert_eq!(
            scope.classify(
                &repo.join("Cargo.toml").to_string_lossy(),
                ToolEffect::Write
            ),
            Scope::InScope
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn something_else_is_out_of_scope_not_refused() {
        let ws = temp("out");
        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify("/tmp/elsewhere.txt", ToolEffect::Write),
            Scope::OutOfScope
        );
        assert_eq!(
            scope.classify("../../escape.txt", ToolEffect::Write),
            Scope::OutOfScope
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn reading_the_system_is_not_a_crime() {
        let ws = temp("read_sys");
        let scope = scope_in(&ws);
        // The exact read that the old jail refused.
        assert_eq!(
            scope.classify("/etc/hosts", ToolEffect::Read),
            Scope::OutOfScope
        );
        assert_eq!(
            scope.classify("/usr/share/doc/README", ToolEffect::Read),
            Scope::OutOfScope
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn writing_the_system_is_off_limits() {
        let ws = temp("write_sys");
        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify("/etc/hosts", ToolEffect::Write),
            Scope::Critical
        );
        assert_eq!(
            scope.classify("/usr/local/bin/tool", ToolEffect::Write),
            Scope::Critical
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn writing_a_repository_s_metadata_is_off_limits() {
        let ws = temp("gitws");
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify(&ws.join(".git/HEAD").to_string_lossy(), ToolEffect::Write),
            Scope::Critical
        );
        assert_eq!(
            scope.classify(&ws.join(".git/HEAD").to_string_lossy(), ToolEffect::Read),
            Scope::InScope,
            "reading repository metadata is ordinary work"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_symlink_out_of_the_workspace_is_judged_by_where_it_leads() {
        let ws = temp("link_ws");
        let outside = temp("link_out");
        std::fs::write(outside.join("secret"), "s").unwrap();
        let link = ws.join("innocent");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify(&link.join("secret").to_string_lossy(), ToolEffect::Write),
            Scope::OutOfScope,
            "the path reads as in-workspace but leads outside it"
        );
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn the_credential_store_is_off_limits_in_both_directions() {
        let ws = temp("creds");
        let data_root = temp("creds_root");
        let auth = data_root.join("auth.json");
        std::fs::write(&auth, "{}").unwrap();
        let scope = scope_in(&ws).protect_data_root(&data_root);

        assert_eq!(
            scope.classify(&auth.to_string_lossy(), ToolEffect::Read),
            Scope::Critical,
            "a credential file must not be readable into the model's context"
        );
        assert_eq!(
            scope.classify(&auth.to_string_lossy(), ToolEffect::Write),
            Scope::Critical
        );
        assert_eq!(
            scope.classify(
                &data_root.join("conversations/x.json").to_string_lossy(),
                ToolEffect::Write
            ),
            Scope::Critical,
            "the agent does not rewrite its own store"
        );
        assert_eq!(
            scope.classify(
                &data_root.join("conversations/x.json").to_string_lossy(),
                ToolEffect::Read
            ),
            Scope::OutOfScope,
            "reading it is still just an out-of-scope read"
        );

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&data_root);
    }

    #[test]
    fn an_exec_call_is_never_classified() {
        let ws = temp("exec");
        let scope = scope_in(&ws);
        // The `cwd` here is inside the workspace, and the command may still write
        // anywhere; classifying it by that argument would be a lie.
        let hit = scope.scope_of(
            "bash",
            ToolEffect::Exec,
            &json!({ "command": "echo x > /etc/hosts", "cwd": ws.to_string_lossy() }),
        );
        assert_eq!(hit, Scope::Unknown);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_tool_with_no_path_argument_is_never_classified() {
        let ws = temp("nopath");
        let scope = scope_in(&ws);
        assert_eq!(
            scope.scope_of("grep", ToolEffect::Read, &json!({ "pattern": "x" })),
            Scope::Unknown
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn the_standard_devices_stay_writable() {
        let ws = temp("dev");
        let scope = scope_in(&ws);
        assert_eq!(
            scope.classify("/dev/null", ToolEffect::Write),
            Scope::OutOfScope
        );
        assert_eq!(
            scope.classify("/dev/tty", ToolEffect::Write),
            Scope::OutOfScope
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn writing_workspace_config_and_mcp_files_is_off_limits() {
        let ws = temp("ws_cfg");
        std::fs::create_dir_all(ws.join(".thunder/plans")).unwrap();
        let scope = scope_in(&ws);

        for protected in [
            ".thunder/config.json",
            ".thunder/config.local.json",
            ".thunder/mcp.json",
            ".thunder/models.json",
            ".thunder/auth.json",
            ".mcp.json",
            "mcp_servers.json",
        ] {
            assert_eq!(
                scope.classify(protected, ToolEffect::Write),
                Scope::Critical,
                "writing {protected} must be Critical"
            );
            assert_eq!(
                scope.classify(protected, ToolEffect::Read),
                Scope::InScope,
                "reading {protected} stays InScope"
            );
        }

        // Plan files and project memory under .thunder remain writable in scope.
        assert_eq!(
            scope.classify(".thunder/plans/session.md", ToolEffect::Write),
            Scope::InScope
        );
        assert_eq!(
            scope.classify(".thunder/THUNDER.md", ToolEffect::Write),
            Scope::InScope
        );

        let _ = std::fs::remove_dir_all(&ws);
    }
}
