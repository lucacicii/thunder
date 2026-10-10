//! Where thunder keeps its per-user state, and the single place that decides it.
//!
//! `THUNDER_CONFIG_DIR` is thunder's **user data root**: the directory holding
//! everything thunder owns for this user. A host application that embeds
//! thunder (an Electron app, a scheduler, a test harness) points this at a
//! directory of its own and gets a fully isolated instance — no writes and no
//! reads under the end user's `~/.thunder`.
//!
//! ```text
//! <root>/models.json           provider registry
//! <root>/auth.json             credentials
//! <root>/models_metadata.json  provider metadata cache        (written)
//! <root>/config.json           user config
//! <root>/mcp.json              MCP servers
//! <root>/THUNDER.md            user-global memory
//! <root>/bridge/               pi-ai bridge install           (written)
//! <root>/plugins/              script plugins                 (written)
//! <root>/skills/               user skills
//! <root>/scratchpad/           oversized tool output          (written)
//! <root>/conversations/        session store                  (written)
//! ```
//!
//! Resolution order: `$THUNDER_CONFIG_DIR` → `<home>/.thunder` → `<temp>/.thunder`.
//! The binaries accept `--config-dir <path>`, which they apply by setting the
//! environment variable before anything else runs — so every layer below,
//! including the Node bridge child they spawn, sees exactly one value.
//!
//! **Deliberately not under the root** (use [`real_home_dir`] for these):
//! `~/.agents`, `~/.pi`, the shell rc that holds API keys, the cross-tool
//! `mcp.json` locations, and `~` expansion for shell paths. Those belong to the
//! user rather than to thunder, so relocating thunder's state must not move
//! them. A host that wants the cross-tool prompt surfaces gone sets
//! `THUNDER_SKILLS_NO_GLOBAL` / `THUNDER_NO_GLOBAL_INSTRUCTIONS`; it never
//! fakes `HOME`.

use std::path::PathBuf;

/// Environment variable naming thunder's user data root. See the module docs.
pub const THUNDER_CONFIG_DIR_ENV: &str = "THUNDER_CONFIG_DIR";

/// Name of the default root inside the user's home directory.
pub const THUNDER_DIR_NAME: &str = ".thunder";

/// The user data root for this process.
pub fn thunder_config_dir() -> PathBuf {
    thunder_config_dir_from(|key| std::env::var(key).ok())
}

/// [`thunder_config_dir`] with an injected environment lookup.
///
/// Tests use this instead of `std::env::set_var`: that call is process-global,
/// and `cargo test` runs a binary's test functions on many threads at once.
pub fn thunder_config_dir_from(vars: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(dir) = non_empty(vars(THUNDER_CONFIG_DIR_ENV)) {
        return PathBuf::from(dir);
    }
    match real_home_dir_from(&vars) {
        Some(home) => home.join(THUNDER_DIR_NAME),
        // No home at all (a container, an unusual service manager): keep state
        // under the temp dir rather than the cwd — a relative root would
        // scatter it wherever the process happens to be started from.
        None => std::env::temp_dir().join(THUNDER_DIR_NAME),
    }
}

/// The real OS home directory (`$HOME`, else `$USERPROFILE`).
///
/// Only for paths that belong to the *user* rather than to thunder — see the
/// module docs. Never returns the thunder data root.
pub fn real_home_dir() -> Option<PathBuf> {
    real_home_dir_from(|key| std::env::var(key).ok())
}

/// [`real_home_dir`] with an injected environment lookup.
pub fn real_home_dir_from(vars: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    non_empty(vars("HOME"))
        .or_else(|| non_empty(vars("USERPROFILE")))
        .map(PathBuf::from)
}

/// A path under the user data root, e.g. `thunder_subdir("bridge")`.
///
/// File *names* stay with the module that owns them; this only owns the root.
pub fn thunder_subdir(name: &str) -> PathBuf {
    thunder_config_dir().join(name)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fake environment, so these tests never touch the process environment.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn data_root_env_wins_over_home() {
        let dir = thunder_config_dir_from(env(&[
            (THUNDER_CONFIG_DIR_ENV, "/data/thunder"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(dir, PathBuf::from("/data/thunder"));
    }

    #[test]
    fn data_root_defaults_to_home() {
        let dir = thunder_config_dir_from(env(&[("HOME", "/home/u")]));
        assert_eq!(dir, PathBuf::from("/home/u").join(THUNDER_DIR_NAME));
    }

    #[test]
    fn data_root_falls_back_to_userprofile() {
        let dir = thunder_config_dir_from(env(&[("USERPROFILE", "/users/u")]));
        assert_eq!(dir, PathBuf::from("/users/u").join(THUNDER_DIR_NAME));
    }

    #[test]
    fn data_root_falls_back_to_temp_without_any_home() {
        let dir = thunder_config_dir_from(env(&[]));
        assert_eq!(dir, std::env::temp_dir().join(THUNDER_DIR_NAME));
    }

    #[test]
    fn blank_env_values_count_as_unset() {
        let dir =
            thunder_config_dir_from(env(&[(THUNDER_CONFIG_DIR_ENV, "   "), ("HOME", "/home/u")]));
        assert_eq!(dir, PathBuf::from("/home/u").join(THUNDER_DIR_NAME));
    }

    #[test]
    fn real_home_ignores_the_data_root() {
        let home = real_home_dir_from(env(&[
            (THUNDER_CONFIG_DIR_ENV, "/data/thunder"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(home, Some(PathBuf::from("/home/u")));
    }

    #[test]
    fn real_home_is_none_when_no_home_exists() {
        assert_eq!(real_home_dir_from(env(&[])), None);
    }
}
