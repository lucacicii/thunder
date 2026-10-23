//! Well-known locations shared by `thunderctl` and the Agent Resume desktop app.
//!
//! The desktop side resolves its sidecar through `daemonResolver.ts`; the paths
//! here mirror that resolver's inputs so `thunderctl status` can explain what the
//! desktop is about to do without duplicating guesswork.

use std::path::{Path, PathBuf};

/// Binaries the desktop sidecar layout needs (MCP std server + TUI + daemon).
pub const DESKTOP_PACKAGES: [&str; 3] = ["thunder-std", "thunder-tui", "thunder-daemon"];

/// Every binary `thunderctl install` publishes, in the workspace package order that
/// produces them (`thunderctl` ships itself so the install stays self-hosting).
pub const ALL_PACKAGES: [&str; 5] = [
    "thunder-core",
    "thunder-std",
    "thunder-tui",
    "thunder-daemon",
    "thunder-cli",
];

/// Installed binary file names, in `ALL_PACKAGES` order.
pub const ALL_BINARIES: [&str; 5] = [
    "thunder",
    "thunder-std",
    "thunder-tui",
    "thunder-daemon",
    "thunderctl",
];

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.cargo/bin` — the single install location for the production binaries.
pub fn install_dir() -> PathBuf {
    match std::env::var_os("THUNDER_INSTALL_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home_dir().join(".cargo").join("bin"),
    }
}

/// `~/.agent-resume-panel` unless `settings.json` relocates the panel home.
pub fn panel_home() -> PathBuf {
    let settings = home_dir().join(".agent-resume-panel").join("settings.json");
    if let Ok(text) = std::fs::read_to_string(&settings) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
            let relocated = json
                .get("panelHome")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(raw) = relocated {
                return expand_tilde(raw);
            }
        }
    }
    home_dir().join(".agent-resume-panel")
}

/// The file the desktop reads `thunder.{repoPath,daemonPath,tuiPath}` from.
pub fn desktop_settings_file() -> PathBuf {
    panel_home().join("settings.desktop.json")
}

/// Thunder data root (conversations, logs, scratchpad, models, auth).
pub fn data_root() -> PathBuf {
    thunder_protocol::thunder_config_dir()
}

/// `<data_root>/state` — per-process runtime markers written by `thunder-daemon`.
pub fn runtime_state_dir() -> PathBuf {
    data_root().join("state")
}

pub fn runtime_state_file(pid: u32) -> PathBuf {
    runtime_state_dir().join(format!("daemon-{pid}.json"))
}

/// macOS app bundles that may ship a bundled `thunder/bin/*` sidecar.
///
/// The bundled layout outranks every repository checkout in the desktop resolver,
/// which is exactly why `thunderctl status` reports it.
pub fn app_bundles() -> Vec<PathBuf> {
    if let Some(raw) = std::env::var_os("THUNDER_APP_BUNDLE") {
        let path = expand_tilde(&raw.to_string_lossy());
        return vec![path.join("Contents").join("Resources")];
    }
    let mut out = Vec::new();
    for app in [
        "/Applications/Agent Resume.app",
        "/Applications/Thunder.app",
    ] {
        let resources = Path::new(app).join("Contents").join("Resources");
        if resources.is_dir() {
            out.push(resources);
        }
    }
    out
}

/// Repository checkouts the desktop probes under `$HOME`.
pub fn local_checkout_candidates() -> Vec<PathBuf> {
    let home = home_dir();
    [
        "wz/thunder",
        "wz/GitHub/thunder",
        "GitHub/thunder",
        "Documents/GitHub/thunder",
        "thunder",
    ]
    .iter()
    .map(|rel| home.join(rel))
    .collect()
}

pub fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(raw)
}

/// Walk up from `start` looking for the thunder workspace root.
pub fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file() {
            if let Ok(text) = std::fs::read_to_string(&manifest) {
                if text.contains("crates/thunder-daemon") || text.contains("thunder-cli") {
                    return Some(dir.to_path_buf());
                }
            }
        }
        current = dir.parent();
    }
    None
}
