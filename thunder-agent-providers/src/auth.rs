use crate::error::ProviderError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthFile {
    #[serde(flatten)]
    pub entries: HashMap<String, serde_json::Value>,
}

impl AuthFile {
    pub fn default_path() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("THUNDER_CONFIG_DIR") {
            return Some(PathBuf::from(dir).join("auth.json"));
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".thunder/auth.json"))
    }

    pub async fn load_default() -> Self {
        let Some(path) = Self::default_path() else {
            return Self::default();
        };
        match tokio::fs::read_to_string(path).await {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn api_key_for(&self, provider: &str) -> Option<String> {
        let value = self.entries.get(provider)?;
        if let Some(key) = value.as_str() {
            return Some(key.to_string());
        }
        value
            .get("key")
            .or_else(|| value.get("apiKey"))
            .or_else(|| value.get("access"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }
}

pub fn resolve_env_value(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let var_name = if let Some(name) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}'))
    {
        Some(name)
    } else {
        trimmed
            .strip_prefix('$')
            .filter(|&name| !name.is_empty() && !name.starts_with('!'))
    };

    if let Some(name) = var_name {
        if let Ok(val) = std::env::var(name) {
            if !val.trim().is_empty() {
                return Some(val);
            }
        }
        // Fallback: check shell profile configuration files (macOS GUI apps don't inherit interactive shell rc)
        if let Some(val) = read_var_from_shell_rc(name) {
            return Some(val);
        }
        return None;
    }

    Some(trimmed.to_string())
}

fn read_var_from_shell_rc(var_name: &str) -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let home_path = std::path::Path::new(&home);
    let files = [
        home_path.join(".zshrc"),
        home_path.join(".zshenv"),
        home_path.join(".zprofile"),
        home_path.join(".bash_profile"),
        home_path.join(".bashrc"),
        home_path.join(".profile"),
    ];

    let pattern = format!("export {var_name}=");
    let pattern2 = format!("{var_name}=");

    for file in &files {
        if let Ok(content) = std::fs::read_to_string(file) {
            for line in content.lines() {
                let trimmed = line.trim();
                let rest = if let Some(r) = trimmed.strip_prefix(&pattern) {
                    Some(r.trim())
                } else {
                    trimmed.strip_prefix(&pattern2).map(|r| r.trim())
                };

                if let Some(val_str) = rest {
                    let cleaned = val_str.trim_matches(|c: char| c == '"' || c == '\'').trim();
                    if !cleaned.is_empty() {
                        return Some(cleaned.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn env_api_key(provider: &str) -> Option<String> {
    if let Ok(value) = std::env::var("THUNDER_API_KEY") {
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    let keys = match provider {
        "openai" => &["OPENAI_API_KEY"][..],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "google" | "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        _ => &[],
    };
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

pub fn resolve_provider_key(
    provider: &str,
    configured: Option<&str>,
    auth: &AuthFile,
) -> Result<Option<String>, ProviderError> {
    if let Some(raw) = configured {
        if let Some(resolved) = resolve_env_value(raw) {
            if !resolved.is_empty() {
                return Ok(Some(resolved));
            }
        }
    }
    if let Some(from_file) = auth.api_key_for(provider) {
        return Ok(Some(from_file));
    }
    Ok(env_api_key(provider))
}
