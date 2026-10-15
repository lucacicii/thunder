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
        let dir = thunder_agent_loop::core::paths::thunder_config_dir();
        Some(Self {
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

    pub fn default_chain(workspace: Option<&std::path::Path>) -> Vec<Self> {
        let mut sources = Vec::new();
        if let Some(src) = Self::thunder_user() {
            sources.push(src);
        }
        sources.push(Self::thunder_project(workspace));
        sources
    }

    /// The file a write must land in so the next merged read observes it.
    ///
    /// Read order is priority order (first source wins per key), so a write is
    /// only visible when it goes to the highest-priority file the reader will
    /// consult for that key: the first chain file that already defines the
    /// target provider, else the first file that already exists, else the user
    /// file (which always resolves).
    pub fn primary_write_path_in_chain(sources: &[ConfigSource]) -> PathBuf {
        for source in sources {
            if source.models_path.exists() {
                return source.models_path.clone();
            }
        }
        sources
            .first()
            .map(|source| source.models_path.clone())
            .unwrap_or_else(|| {
                thunder_agent_loop::core::paths::thunder_config_dir().join("models.json")
            })
    }

    /// Write target for one provider's probe result: the first chain file (read
    /// order) that defines `provider_id`, so the merged view — which takes the
    /// whole provider object from the first definer — picks the update up.
    /// Falls back to [`Self::primary_write_path_in_chain`] when no file
    /// defines the provider yet; the caller then no-ops unless it created the
    /// entry (today it only patches existing entries).
    pub async fn owner_path_in_chain(sources: &[ConfigSource], provider_id: &str) -> PathBuf {
        for source in sources {
            if !source.models_path.exists() {
                continue;
            }
            let Ok(raw) = tokio::fs::read_to_string(&source.models_path).await else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            if value
                .get("providers")
                .and_then(|providers| providers.get(provider_id))
                .is_some()
            {
                return source.models_path.clone();
            }
        }
        Self::primary_write_path_in_chain(sources)
    }

    /// [`Self::primary_write_path_in_chain`] for the ambient chain (process
    /// working directory as the project).
    pub fn primary_write_path(workspace: Option<&std::path::Path>) -> PathBuf {
        Self::primary_write_path_in_chain(&Self::default_chain(workspace))
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
            if models.utility_model.is_none() && file.utility_model.is_some() {
                models.utility_model = file.utility_model;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn chain_in(user_dir: &std::path::Path, project_dir: &std::path::Path) -> Vec<ConfigSource> {
        vec![
            ConfigSource {
                name: "thunder-user".to_string(),
                models_path: user_dir.join("models.json"),
                auth_path: user_dir.join("auth.json"),
            },
            ConfigSource {
                name: "thunder-project".to_string(),
                models_path: project_dir.join("models.json"),
                auth_path: project_dir.join("auth.json"),
            },
        ]
    }

    fn write_models(path: &std::path::Path, raw: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, raw).unwrap();
    }

    #[test]
    fn write_goes_to_first_existing_in_read_order() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let chain = chain_in(user.path(), project.path());
        // Nothing exists yet: the user file is where it would be created.
        assert_eq!(
            ConfigSource::primary_write_path_in_chain(&chain),
            user.path().join("models.json")
        );
        // Only the project file exists: write where the reader looks.
        write_models(&project.path().join("models.json"), "{}");
        assert_eq!(
            ConfigSource::primary_write_path_in_chain(&chain),
            project.path().join("models.json")
        );
        // Both exist: the read winner (user first) is the write target, so a
        // persisted value cannot be shadowed on the next merged load.
        write_models(&user.path().join("models.json"), "{}");
        assert_eq!(
            ConfigSource::primary_write_path_in_chain(&chain),
            user.path().join("models.json")
        );
    }

    #[tokio::test]
    async fn owner_is_the_first_definer_in_read_order() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_models(
            &user.path().join("models.json"),
            r#"{"providers": {"acme": {"models": [{"id": "a"}]}}}"#,
        );
        write_models(
            &project.path().join("models.json"),
            r#"{"providers": {"acme": {"models": [{"id": "a"}]}, "beta": {"models": [{"id": "b"}]}}}"#,
        );
        let chain = chain_in(user.path(), project.path());
        // `acme` is defined in both: the merged view takes the user's object.
        assert_eq!(
            ConfigSource::owner_path_in_chain(&chain, "acme").await,
            user.path().join("models.json")
        );
        // `beta` lives only in the project file.
        assert_eq!(
            ConfigSource::owner_path_in_chain(&chain, "beta").await,
            project.path().join("models.json")
        );
        // Unknown provider: fall back to the primary write path (user file).
        assert_eq!(
            ConfigSource::owner_path_in_chain(&chain, "nope").await,
            user.path().join("models.json")
        );
    }

    #[tokio::test]
    async fn write_target_is_visible_to_the_merged_read() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_models(
            &user.path().join("models.json"),
            r#"{"utilityModel": "user/fast", "providers": {}}"#,
        );
        write_models(
            &project.path().join("models.json"),
            r#"{"utilityModel": "project/slow", "providers": {}}"#,
        );
        let chain = chain_in(user.path(), project.path());
        // Simulate what update_utility_model_config does: write the new value
        // to the primary write path, then re-read through the merge.
        let target = ConfigSource::primary_write_path_in_chain(&chain);
        let mut value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        value["utilityModel"] = serde_json::json!("user/new");
        std::fs::write(&target, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let (merged, _) = load_merged(&chain).await.unwrap();
        assert_eq!(merged.utility_model.as_deref(), Some("user/new"));
    }
}
