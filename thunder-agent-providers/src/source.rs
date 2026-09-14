use crate::auth::AuthFile;
use crate::config::ModelsFile;
use crate::error::ProviderError;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSource {
    pub name: String,
    pub models_path: PathBuf,
    pub auth_path: PathBuf,
}

impl ConfigSource {
    pub fn thunder_user() -> Option<Self> {
        let root = std::env::var("THUNDER_CONFIG_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".thunder")));
        root.map(|dir| Self {
            name: "thunder-user".to_string(),
            models_path: dir.join("models.json"),
            auth_path: dir.join("auth.json"),
        })
    }

    pub fn thunder_project(workspace: Option<&std::path::Path>) -> Self {
        let dir = workspace
            .map(|p| p.join(".thunder"))
            .unwrap_or_else(|| PathBuf::from(".thunder"));
        Self {
            name: "thunder-project".to_string(),
            models_path: dir.join("models.json"),
            auth_path: dir.join("auth.json"),
        }
    }

    pub fn pi_project(workspace: Option<&std::path::Path>) -> Self {
        let dir = workspace
            .map(|p| p.join(".pi"))
            .unwrap_or_else(|| PathBuf::from(".pi"));
        Self {
            name: "pi-project".to_string(),
            models_path: dir.join("models.json"),
            auth_path: dir.join("auth.json"),
        }
    }

    pub fn pi_user() -> Option<Self> {
        std::env::var("HOME").ok().map(|home| {
            let dir = PathBuf::from(home).join(".pi/agent");
            Self {
                name: "pi-user".to_string(),
                models_path: dir.join("models.json"),
                auth_path: dir.join("auth.json"),
            }
        })
    }

    pub fn default_chain(workspace: Option<&std::path::Path>) -> Vec<Self> {
        let mut sources = Vec::new();
        if let Some(src) = Self::thunder_user() {
            sources.push(src);
        }
        sources.push(Self::thunder_project(workspace));
        sources.push(Self::pi_project(workspace));
        if let Some(src) = Self::pi_user() {
            sources.push(src);
        }
        sources
    }

    pub async fn load_models(&self) -> Option<ModelsFile> {
        if !self.models_path.exists() {
            return None;
        }
        ModelsFile::load_path(&self.models_path).await.ok()
    }

    pub async fn load_auth(&self) -> AuthFile {
        if !self.auth_path.exists() {
            return AuthFile::default();
        }
        tokio::fs::read_to_string(&self.auth_path)
            .await
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }
}

pub async fn load_merged(
    sources: &[ConfigSource],
) -> Result<(ModelsFile, AuthFile), ProviderError> {
    let mut models = ModelsFile::default();
    let mut auth = AuthFile::default();
    for source in sources {
        if let Some(file) = source.load_models().await {
            for (id, provider) in file.providers {
                models.providers.entry(id).or_insert(provider);
            }
        }
        let file_auth = source.load_auth().await;
        for (id, value) in file_auth.entries {
            auth.entries.entry(id).or_insert(value);
        }
    }
    Ok((models, auth))
}
