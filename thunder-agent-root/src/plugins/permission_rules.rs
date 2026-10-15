//! What a remembered rule matches on, and where the remembered rules live.
//!
//! A rule only helps if the *next* form of the same call matches it, and the
//! model writes the same target in more than one way: `a.txt`, `./a.txt`,
//! `/ws/a.txt`, `~/notes.md` are one file, and `git  status` (two spaces) is one
//! command. A rule that stored what the dialog happened to contain would pay for
//! exactly one spelling and ask again for the rest — which reads to a user as
//! "always allow does not work".
//!
//! So a rule is stored *normalised*: the command with its whitespace collapsed,
//! the path absolute where the workspace is known, `.`/`..` resolved textually
//! and no trailing separator. [`AllowRule`](super::mode::AllowRule) therefore
//! normalises both sides — what it records and what it compares — through this
//! module, and the store below writes the normalised list to disk so it outlives
//! the process that agreed to it.
//!
//! Persisting a grant is a real change in what "always allow" means, and it is
//! deliberately *user-level*: `<data root>/permissions.json`, the same file for
//! every workspace this user opens. The reasoning is that the alternative —
//! asking the same question again in the next project — is what makes people
//! turn the question off altogether. The rules stay narrow (one tool, one
//! prefix), they stay ignorable in plan mode, and the guard still runs first.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thunder_agent_loop::core::paths::{real_home_dir, thunder_config_dir};

use super::mode::AllowRule;

/// The file's name inside thunder's data root (`--config-dir`, else `~/.thunder`).
const RULES_FILE_NAME: &str = "permissions.json";

/// The format this code writes. A file claiming another version is not read:
/// guessing at a shape thunder did not write is how a grant gets misread as a
/// broader one than the user gave.
pub const RULES_VERSION: u32 = 1;

/// The document as written to disk.
///
/// Deliberately an object with a version rather than a bare array: the first
/// change to what a rule *is* gets a migration instead of a silent misreading.
#[derive(Debug, Serialize, Deserialize)]
struct RulesDocument {
    version: u32,
    rules: Vec<AllowRule>,
}

/// Where the standing rules live.
pub fn rules_path() -> PathBuf {
    thunder_config_dir().join(RULES_FILE_NAME)
}

/// Read the standing rules.
///
/// A missing, unreadable, corrupt or future-versioned file is an empty list. A
/// permission file is not worth failing a run over, and every one of those cases
/// has the same safe reading — this file only ever *grants*, so failing to read
/// it means granting nothing and asking, which is where the user started.
pub fn load(path: &Path) -> Vec<AllowRule> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_json::from_str::<RulesDocument>(&text) {
        Ok(document) if document.version == RULES_VERSION => document.rules,
        _ => Vec::new(),
    }
}

/// Replace the standing rules on disk.
///
/// Written to a sibling file and renamed over the target: a crash mid-write must
/// not leave a truncated file, because the reader treats "unparseable" as "no
/// rules" and a half-written grant would silently become no grant at all. The
/// rename is atomic on every platform thunder ships on.
///
/// The file is left readable only by its owner where the platform has modes: it
/// is a list of what the agent may do without asking, and that is nobody else's
/// business.
pub fn save(path: &Path, rules: &[AllowRule]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let document = RulesDocument {
        version: RULES_VERSION,
        rules: rules.to_vec(),
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temporary, path)
}

/// Add `rules` to what is already on disk, and leave the union there.
///
/// A plain replace would let one of two live sessions silently drop the other's
/// grant: each plugin holds the list it read when it was created, so whichever
/// writes last decides the whole file. Merging makes the file the union of
/// everything every session has been told, which is what somebody opening
/// `permissions.json` expects to find.
///
/// The read-modify-write is not a transaction, and two *processes* can still
/// interleave; a single data root is a single daemon in every deployment thunder
/// ships, and the window here is one small file wide.
pub fn merge(path: &Path, rules: &[AllowRule]) -> std::io::Result<()> {
    let mut merged = load(path);
    for rule in rules {
        if !merged.contains(rule) {
            merged.push(rule.clone());
        }
    }
    save(path, &merged)
}

/// A command as a rule and a call agree on it: runs of whitespace collapsed to
/// one space, trimmed, at both ends.
///
/// Whitespace is the one difference that is never a difference in meaning, and
/// it is the one a model introduces freely (`git  status`, a trailing space).
/// Collapsing it cannot create a chaining character or change the verb, so this
/// never widens what a rule approves.
pub fn normalize_command(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A path as a rule and a call agree on it.
///
/// `workspace` is what makes the relative and absolute spellings of one file
/// comparable: with it bound, `src/main.rs` is stored as `/ws/src/main.rs`, so a
/// rule recorded from one spelling pays for the other. Without it (an embedder
/// that never bound a workspace) the path is only cleaned up textually, and the
/// two spellings stay distinct — a rule that fails to match asks a question,
/// which is the failure this design is allowed to have.
pub fn normalize_path(raw: &str, workspace: Option<&Path>) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    let expanded = expand_home(raw);
    let path = Path::new(&expanded);
    let joined = match (workspace, path.is_absolute()) {
        (Some(workspace), false) => workspace.join(path),
        _ => path.to_path_buf(),
    };
    lexical(&joined)
}

/// Whether `path` is `prefix` itself or lies beneath it.
///
/// Separator-aware rather than `starts_with(&format!("{prefix}/"))`: a rule for
/// `src` must not pay for `srcfile`, a rule for `/ws/src/` must behave like the
/// one for `/ws/src`, and a rule for the filesystem root has to cover what is
/// under it instead of only itself.
pub fn under(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    if prefix.is_empty() {
        return false;
    }
    if prefix == "/" {
        return path.starts_with('/');
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.starts_with('/') || rest.starts_with('\\'),
        None => false,
    }
}

/// `~` as the user's home directory.
///
/// [`real_home_dir`] rather than the data root: a rule about the user's files is
/// about the user's home, and it stays the same rule when a host moves thunder's
/// own state somewhere else. `~otheruser/...` is left alone — this cannot
/// resolve it, and leaving it unmatched only costs a question.
fn expand_home(raw: &str) -> String {
    let Some(rest) = raw.strip_prefix('~') else {
        return raw.to_string();
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return raw.to_string();
    }
    match real_home_dir() {
        Some(home) => format!("{}{}", home.to_string_lossy(), rest),
        None => raw.to_string(),
    }
}

/// Resolve `.`, `..`, doubled separators and a trailing separator, without
/// touching the filesystem.
///
/// Textual on purpose: the path a rule names may not exist any more by the time
/// it is matched, and a canonicalising walk would turn "unreadable" into "asks
/// every time". Resolving `..` here is also what keeps a rule honest — a call
/// naming `/ws/../etc/passwd` normalises to `/etc/passwd` and does not inherit
/// the authority of a rule about `/ws`.
fn lexical(path: &Path) -> String {
    let mut out = PathBuf::new();
    let mut owned = 0usize;
    let absolute = path.is_absolute();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if owned > 0 {
                    out.pop();
                    owned -= 1;
                } else if !absolute {
                    // A leading `..` in a relative path has nothing to cancel,
                    // and dropping it would quietly name a different file.
                    out.push("..");
                }
            }
            Component::RootDir | Component::Prefix(_) => out.push(component.as_os_str()),
            Component::Normal(part) => {
                out.push(part);
                owned += 1;
            }
        }
    }

    if out.as_os_str().is_empty() {
        return ".".to_string();
    }
    out.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_agree_across_whitespace() {
        assert_eq!(normalize_command("git  status"), "git status");
        assert_eq!(normalize_command("  git status \n"), "git status");
        // Runs inside quotes collapse too. That is the point rather than a
        // drawback: both sides go through here, so they agree either way, and
        // collapsing cannot introduce a separator or change the verb.
        assert_eq!(
            normalize_command("grep -rn \"a  b\" src/"),
            "grep -rn \"a b\" src/"
        );
    }

    #[test]
    fn paths_are_absolute_when_a_workspace_is_bound() {
        let ws = Path::new("/ws");
        for raw in ["src/main.rs", "./src/main.rs", "/ws/src/main.rs"] {
            assert_eq!(normalize_path(raw, Some(ws)), "/ws/src/main.rs", "{raw}");
        }
    }

    #[test]
    fn paths_lose_their_dot_slash_and_trailing_separator() {
        let ws = Path::new("/ws");
        assert_eq!(normalize_path("src/", Some(ws)), "/ws/src");
        assert_eq!(normalize_path("src//main.rs", Some(ws)), "/ws/src/main.rs");
        assert_eq!(normalize_path("src/./main.rs", Some(ws)), "/ws/src/main.rs");
        // The trailing separator is what used to make a directory rule build
        // `src//` and never match a file beneath it.
        assert!(under("/ws/src/main.rs", &normalize_path("src/", Some(ws))));
    }

    #[test]
    fn parent_components_resolve_textually() {
        let ws = Path::new("/ws");
        assert_eq!(normalize_path("../etc/passwd", Some(ws)), "/etc/passwd");
        assert_eq!(normalize_path("a/b/../c", Some(ws)), "/ws/a/c");
        // Without a workspace, a relative path keeps what it can still say.
        assert_eq!(normalize_path("../x", None), "../x");
        assert_eq!(normalize_path("./x", None), "x");
    }

    #[test]
    fn a_rule_about_one_directory_does_not_cover_its_sibling() {
        assert!(under("/ws/src/main.rs", "/ws/src"));
        assert!(under("/ws/src", "/ws/src"));
        assert!(!under("/ws/srcfile", "/ws/src"));
        assert!(!under("/ws/other", "/ws/src"));
    }

    #[test]
    fn a_rule_about_the_root_covers_what_is_under_it() {
        assert!(under("/etc/passwd", "/"));
        assert!(under("/", "/"));
        assert!(!under("relative", "/"));
    }

    fn rule(tool: &str, prefix: &str) -> AllowRule {
        AllowRule {
            tool: tool.to_string(),
            arg_prefix: Some(prefix.to_string()),
        }
    }

    fn temporary(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thunder-rules-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join(RULES_FILE_NAME)
    }

    #[test]
    fn rules_survive_a_round_trip() {
        let path = temporary("round-trip");
        let rules = vec![rule("bash", "git status"), rule("read_file", "/ws/a.txt")];
        save(&path, &rules).expect("write");

        assert_eq!(load(&path), rules);
        // The temporary sibling is renamed onto the target, not left behind.
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_missing_corrupt_or_future_file_grants_nothing() {
        let path = temporary("unreadable");
        assert_eq!(load(&path), Vec::new(), "missing file");

        std::fs::write(&path, "not json at all").expect("write");
        assert_eq!(load(&path), Vec::new(), "corrupt file");

        std::fs::write(&path, r#"{"version":99,"rules":[{"tool":"bash"}]}"#).expect("write");
        assert_eq!(load(&path), Vec::new(), "a version this code did not write");

        std::fs::write(
            &path,
            r#"{"version":1,"rules":[{"tool":"bash","arg_prefix":"ls"}]}"#,
        )
        .expect("write");
        assert_eq!(load(&path), vec![rule("bash", "ls")]);
    }

    #[test]
    fn saving_an_empty_list_is_a_real_write() {
        let path = temporary("cleared");
        save(&path, &[rule("bash", "ls")]).expect("write");
        save(&path, &[]).expect("write");
        assert_eq!(load(&path), Vec::new());
    }
}
