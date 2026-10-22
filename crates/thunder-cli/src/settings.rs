//! Read/write the desktop's `settings.desktop.json` `thunder` object.
//!
//! The desktop store (`@agent-resume/core` `settings/store.ts`) explicitly merges
//! `thunder.{repoPath,daemonPath,tuiPath}` from disk on every load, so an external
//! writer is supported by design. We still back up and write atomically, and we
//! never touch any other key.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::paths;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThunderSettings {
    pub repo_path: Option<String>,
    pub daemon_path: Option<String>,
    pub tui_path: Option<String>,
}

impl ThunderSettings {
    pub fn from_value(value: &Value) -> Self {
        let read = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Self {
            repo_path: read("repoPath"),
            daemon_path: read("daemonPath"),
            tui_path: read("tuiPath"),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.repo_path.is_none() && self.daemon_path.is_none() && self.tui_path.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct DesktopSettings {
    pub file: PathBuf,
    pub root: Value,
}

impl DesktopSettings {
    pub fn load() -> Result<Self, String> {
        Self::load_from(&paths::desktop_settings_file())
    }

    pub fn load_from(file: &Path) -> Result<Self, String> {
        let root = match std::fs::read_to_string(file) {
            Ok(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(&text)
                .map_err(|e| format!("{} is not valid JSON: {e}", file.display()))?,
            _ => json!({}),
        };
        if !root.is_object() {
            return Err(format!("{} does not contain a JSON object", file.display()));
        }
        Ok(Self {
            file: file.to_path_buf(),
            root,
        })
    }

    pub fn thunder(&self) -> ThunderSettings {
        self.root
            .get("thunder")
            .map(ThunderSettings::from_value)
            .unwrap_or_default()
    }

    /// Replace the `thunder` object. `None` removes the key entirely, which hands
    /// control back to the bundled sidecar (the `bundled` mode).
    pub fn apply(&mut self, next: Option<&ThunderSettings>) {
        let object = self.root.as_object_mut().expect("checked in load_from");
        match next {
            Some(settings) if !settings.is_empty() => {
                let mut thunder = Map::new();
                if let Some(repo) = &settings.repo_path {
                    thunder.insert("repoPath".into(), Value::String(repo.clone()));
                }
                if let Some(bin) = &settings.daemon_path {
                    thunder.insert("daemonPath".into(), Value::String(bin.clone()));
                }
                if let Some(tui) = &settings.tui_path {
                    thunder.insert("tuiPath".into(), Value::String(tui.clone()));
                }
                object.insert("thunder".into(), Value::Object(thunder));
            }
            _ => {
                object.remove("thunder");
            }
        }
    }

    pub fn to_pretty(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.root)
            .map(|text| format!("{text}\n"))
            .map_err(|e| format!("failed to serialize settings: {e}"))
    }

    /// Atomic write with a one-generation backup next to the original.
    pub fn save(&self) -> Result<PathBuf, String> {
        let text = self.to_pretty()?;
        let parent = self
            .file
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", self.file.display()))?;
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;

        let backup = self.file.with_extension("json.thunderctl.bak");
        let existing_permissions = std::fs::metadata(&self.file)
            .ok()
            .map(|meta| meta.permissions());
        if self.file.is_file() {
            std::fs::copy(&self.file, &backup)
                .map_err(|e| format!("backup {}: {e}", backup.display()))?;
        }

        let tmp = self.file.with_extension("json.thunderctl.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        // The desktop settings carry provider API keys: never widen the mode of a
        // file the user (or the app) deliberately created as 0600.
        if let Some(permissions) = existing_permissions {
            let _ = std::fs::set_permissions(&tmp, permissions);
        }
        std::fs::rename(&tmp, &self.file)
            .map_err(|e| format!("rename {}: {e}", self.file.display()))?;
        Ok(backup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_preserves_unrelated_keys_and_replaces_thunder() {
        let mut settings = DesktopSettings {
            file: PathBuf::from("/tmp/unused.json"),
            root: json!({
                "panelHome": "~/.agent-resume-panel",
                "thunder": { "daemonPath": "/old/daemon", "repoPath": "/old/repo" },
                "desktop": { "theme": "light" }
            }),
        };

        settings.apply(Some(&ThunderSettings {
            repo_path: Some("/repo".into()),
            daemon_path: Some("/repo/target/tui-dev/thunder-daemon".into()),
            tui_path: None,
        }));

        assert_eq!(settings.root["panelHome"], json!("~/.agent-resume-panel"));
        assert_eq!(settings.root["desktop"]["theme"], json!("light"));
        assert_eq!(
            settings.root["thunder"]["daemonPath"],
            json!("/repo/target/tui-dev/thunder-daemon")
        );
        assert_eq!(settings.root["thunder"]["repoPath"], json!("/repo"));
        assert!(settings.root["thunder"].get("tuiPath").is_none());
    }

    #[test]
    fn apply_none_removes_the_thunder_key() {
        let mut settings = DesktopSettings {
            file: PathBuf::from("/tmp/unused.json"),
            root: json!({ "thunder": { "daemonPath": "/x" }, "uiLanguage": "zh" }),
        };
        settings.apply(None);
        assert!(settings.root.get("thunder").is_none());
        assert_eq!(settings.root["uiLanguage"], json!("zh"));
    }

    #[test]
    fn save_preserves_file_mode() {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("settings.desktop.json");
        std::fs::write(&file, "{\"uiLanguage\":\"zh\"}\n").expect("seed");
        #[cfg(unix)]
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let mut settings = DesktopSettings::load_from(&file).expect("load");
        settings.apply(None);
        settings.save().expect("save");

        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn roundtrip_through_disk_keeps_thunder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("settings.desktop.json");
        std::fs::write(&file, "{\"uiLanguage\":\"zh\"}\n").expect("seed");

        let mut settings = DesktopSettings::load_from(&file).expect("load");
        settings.apply(Some(&ThunderSettings {
            repo_path: None,
            daemon_path: Some("/installed/thunder-daemon".into()),
            tui_path: Some("/installed/thunder-tui".into()),
        }));
        settings.save().expect("save");

        let reloaded = DesktopSettings::load_from(&file).expect("reload");
        let thunder = reloaded.thunder();
        assert_eq!(
            thunder.daemon_path.as_deref(),
            Some("/installed/thunder-daemon")
        );
        assert_eq!(thunder.tui_path.as_deref(), Some("/installed/thunder-tui"));
        assert!(thunder.repo_path.is_none());
        assert!(tmp
            .path()
            .join("settings.desktop.json.thunderctl.bak")
            .is_file());
    }
}
