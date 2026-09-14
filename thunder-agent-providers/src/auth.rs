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
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".pi/agent/auth.json"))
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
    if let Some(name) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(name).ok();
    }
    if let Some(name) = trimmed.strip_prefix('$') {
        if !name.is_empty() && !name.starts_with('!') {
            return std::env::var(name).ok();
        }
    }
    Some(trimmed.to_string())
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
