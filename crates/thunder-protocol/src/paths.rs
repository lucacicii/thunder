use std::path::PathBuf;

pub const THUNDER_CONFIG_DIR_ENV: &str = "THUNDER_CONFIG_DIR";
pub const THUNDER_DIR_NAME: &str = ".thunder";

pub fn thunder_config_dir() -> PathBuf {
    thunder_config_dir_from(|key| std::env::var(key).ok())
}

pub fn thunder_config_dir_from(vars: impl Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(dir) = non_empty(vars(THUNDER_CONFIG_DIR_ENV)) {
        return PathBuf::from(dir);
    }
    match real_home_dir_from(&vars) {
        Some(home) => home.join(THUNDER_DIR_NAME),
        None => std::env::temp_dir().join(THUNDER_DIR_NAME),
    }
}

pub fn real_home_dir() -> Option<PathBuf> {
    real_home_dir_from(|key| std::env::var(key).ok())
}

pub fn real_home_dir_from(vars: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    non_empty(vars("HOME"))
        .or_else(|| non_empty(vars("USERPROFILE")))
        .map(PathBuf::from)
}

pub fn thunder_subdir(name: &str) -> PathBuf {
    thunder_config_dir().join(name)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}
