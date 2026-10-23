//! Reimplementation of the desktop daemon/TUI discovery chain (`daemonResolver.ts`).
//!
//! Desktop priority, highest first:
//!   1. `THUNDER_DAEMON_BIN` / `THUNDER_TUI_BIN`            (env)
//!   2. `settings.thunder.{daemonPath,tuiPath}`             (explicit)
//!   3. `THUNDER_PATH`                                      (repo)
//!   4. `settings.thunder.repoPath`                         (repo)
//!   5. bundled `<App>.app/Contents/Resources/thunder/bin`   ← beats any checkout
//!   6. `$HOME` checkout guesses
//!
//! `thunderctl status` prints the whole chain, because "why is my build not
//! running" is almost always rule 2 or rule 5.

use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Daemon,
    Tui,
}

impl Kind {
    pub fn binary_name(self) -> &'static str {
        match self {
            Kind::Daemon => "thunder-daemon",
            Kind::Tui => "thunder-tui",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Kind::Daemon => "THUNDER_DAEMON_BIN",
            Kind::Tui => "THUNDER_TUI_BIN",
        }
    }

    pub fn settings_key(self) -> &'static str {
        match self {
            Kind::Daemon => "daemonPath",
            Kind::Tui => "tuiPath",
        }
    }

    pub fn script_name(self) -> &'static str {
        match self {
            Kind::Daemon => "daemon.sh",
            Kind::Tui => "run.sh",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Short label shown in `thunderctl status`, e.g. `settings.thunder.daemonPath`.
    pub source: String,
    /// Resolved binary path (may not exist).
    pub binary: PathBuf,
    /// Repo/build-script fallback the desktop can exec instead of the binary.
    pub script: Option<PathBuf>,
    pub exists: bool,
}

impl Entry {
    pub fn is_runnable(&self) -> bool {
        self.exists || self.script.as_ref().is_some_and(|s| s.is_file())
    }

    pub fn describe(&self) -> String {
        if self.exists {
            self.binary.display().to_string()
        } else if let Some(script) = self.script.as_ref().filter(|s| s.is_file()) {
            format!("{} (script)", script.display())
        } else {
            format!("{} (missing)", self.binary.display())
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resolution {
    pub entries: Vec<Entry>,
    /// What the desktop would spawn right now, if anything.
    pub winner: Option<Entry>,
}

/// Ambient inputs for the chain: env vars, `$HOME` checkouts and app bundles.
///
/// Injected rather than read directly so the discovery rules stay unit-testable on
/// a machine that really does have a checkout and an installed app bundle.
pub struct Probe<'a> {
    pub env: &'a dyn Fn(&str) -> Option<String>,
    pub checkouts: &'a [PathBuf],
    pub bundles: &'a [PathBuf],
}

impl Probe<'static> {
    /// The real machine.
    pub fn ambient() -> (fn(&str) -> Option<String>, Vec<PathBuf>, Vec<PathBuf>) {
        (
            |key: &str| std::env::var(key).ok(),
            paths::local_checkout_candidates(),
            paths::app_bundles(),
        )
    }
}

/// Build the full candidate chain in desktop priority order.
pub fn resolve(
    kind: Kind,
    settings_thunder: Option<&serde_json::Value>,
    repo_hint: Option<&Path>,
) -> Resolution {
    let (env, checkouts, bundles) = Probe::ambient();
    let probe = Probe {
        env: &env,
        checkouts: &checkouts,
        bundles: &bundles,
    };
    resolve_in(&probe, kind, settings_thunder, repo_hint)
}

/// Chain builder against an explicit [`Probe`] (tests pass empty checkouts/bundles).
pub fn resolve_in(
    probe: &Probe<'_>,
    kind: Kind,
    settings_thunder: Option<&serde_json::Value>,
    repo_hint: Option<&Path>,
) -> Resolution {
    let binary_name = kind.binary_name();
    let mut entries: Vec<Entry> = Vec::new();

    // 1. Explicit binaries.
    if let Some(raw) = non_empty((probe.env)(kind.env_var())) {
        entries.push(explicit_binary(format!("env {}", kind.env_var()), &raw));
    }
    if let Some(raw) = settings_thunder
        .and_then(|v| v.get(kind.settings_key()))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        entries.push(explicit_binary(
            format!("settings.thunder.{}", kind.settings_key()),
            raw,
        ));
    }

    // 2. Repository checkouts, in desktop order: THUNDER_PATH, settings.repoPath.
    let mut repos: Vec<(String, PathBuf)> = Vec::new();
    if let Some(raw) = non_empty((probe.env)("THUNDER_PATH")) {
        repos.push(("env THUNDER_PATH".to_string(), paths::expand_tilde(&raw)));
    }
    if let Some(raw) = settings_thunder
        .and_then(|v| v.get("repoPath"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        repos.push((
            "settings.thunder.repoPath".to_string(),
            paths::expand_tilde(raw),
        ));
    }
    for (source, repo) in repos {
        entries.push(repo_entry(&source, &repo, binary_name, kind.script_name()));
    }

    // 3. Bundled sidecar — outranks every `$HOME` guess.
    for resources in probe.bundles {
        for sub in ["thunder/bin", "thunder"] {
            let dir = resources.join(sub);
            if dir.is_dir() {
                entries.push(Entry {
                    source: format!(
                        "bundled ({})",
                        resources.parent().unwrap_or(resources.as_path()).display()
                    ),
                    binary: dir.join(binary_name),
                    script: None,
                    exists: dir.join(binary_name).is_file(),
                });
            }
        }
    }

    // 4. Checkout guesses. The desktop probes `cwd`/`$HOME` *after* the bundle, so a
    //    repository build never outranks an app that ships its own sidecar.
    let mut checkouts: Vec<(&str, &Path)> = Vec::new();
    if let Some(repo) = repo_hint {
        checkouts.push(("thunder checkout", repo));
    }
    for repo in probe.checkouts {
        if repo.is_dir() {
            checkouts.push(("local-checkout", repo.as_path()));
        }
    }
    for (source, repo) in checkouts {
        let release_bin = repo.join("target").join("release").join(binary_name);
        if entries.iter().any(|e| e.binary == release_bin) {
            continue;
        }
        entries.push(repo_entry(source, repo, binary_name, kind.script_name()));
    }

    let winner = entries
        .iter()
        .find(|e| e.exists)
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.script.as_ref().is_some_and(|s| s.is_file()))
        })
        .cloned();

    Resolution { entries, winner }
}

fn explicit_binary(source: String, raw: &str) -> Entry {
    let path = paths::expand_tilde(raw);
    let exists = path.is_file();
    Entry {
        source,
        binary: path,
        script: None,
        exists,
    }
}

fn repo_entry(source: &str, repo: &Path, binary_name: &str, script_name: &str) -> Entry {
    let candidates = [
        repo.join("target").join("release").join(binary_name),
        repo.join("target").join("debug").join(binary_name),
        repo.join("bin").join(binary_name),
        repo.join(binary_name),
    ];
    let binary = candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone());
    let script = repo.join(script_name);
    Entry {
        source: source.to_string(),
        exists: binary.is_file(),
        binary,
        script: if script.is_file() { Some(script) } else { None },
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_daemon_path_wins_over_repo_binary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("target/release")).expect("mkdir");
        let repo_bin = repo.join("target/release/thunder-daemon");
        std::fs::write(&repo_bin, b"repo").expect("write");
        let explicit = tmp.path().join("installed-thunder-daemon");
        std::fs::write(&explicit, b"installed").expect("write");

        let env = |_: &str| None;
        let probe = Probe {
            env: &env,
            checkouts: &[],
            bundles: &[],
        };
        let settings = json!({ "daemonPath": explicit.to_str().expect("utf8") });
        let resolution = resolve_in(&probe, Kind::Daemon, Some(&settings), Some(&repo));
        let winner = resolution.winner.expect("winner");
        assert_eq!(winner.binary, explicit);
    }

    #[test]
    fn repo_entry_falls_back_to_debug_then_script() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("target/debug")).expect("mkdir");
        let debug_bin = repo.join("target/debug/thunder-tui");
        std::fs::write(&debug_bin, b"debug").expect("write");

        let entry = repo_entry("test", &repo, "thunder-tui", "run.sh");
        assert_eq!(entry.binary, debug_bin);
        assert!(entry.exists);

        let empty = repo_entry("test", &tmp.path().join("nowhere"), "thunder-tui", "run.sh");
        assert!(!empty.exists);
        assert!(empty.script.is_none());
    }

    #[test]
    fn script_only_repo_still_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::fs::write(repo.join("daemon.sh"), b"#!/bin/bash\n").expect("write");

        let env = |_: &str| None;
        let probe = Probe {
            env: &env,
            checkouts: &[],
            bundles: &[],
        };
        let resolution = resolve_in(&probe, Kind::Daemon, None, Some(&repo));
        let winner = resolution.winner.expect("script fallback");
        assert!(!winner.exists);
        assert_eq!(winner.script.expect("script"), repo.join("daemon.sh"));
    }
}
