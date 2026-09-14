use super::ConversationStore;
use crate::error::ConversationError;
use crate::types::{Conversation, ConversationFilter, ConversationSummary};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct FsConversationStore {
    root: PathBuf,
    index: Arc<RwLock<HashMap<String, ConversationSummary>>>,
}

impl FsConversationStore {
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, ConversationError> {
        let root = root.into();
        fs::create_dir_all(&root).await?;

        let store = Self {
            root,
            index: Arc::new(RwLock::new(HashMap::new())),
        };

        store.load_or_rebuild_index().await?;
        Ok(store)
    }

    pub fn default_store_root() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".thunder").join("conversations")
        } else {
            std::env::temp_dir().join("thunder-conversations")
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn conv_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn conv_file(&self, id: &str) -> PathBuf {
        self.conv_dir(id).join("conversation.json")
    }

    fn index_file(&self) -> PathBuf {
        self.root.join("index.json")
    }

    async fn load_or_rebuild_index(&self) -> Result<(), ConversationError> {
        let index_path = self.index_file();
        if index_path.exists() {
            match fs::read(&index_path).await {
                Ok(bytes) => match serde_json::from_slice::<HashMap<String, ConversationSummary>>(&bytes) {
                    Ok(loaded) => {
                        *self.index.write().await = loaded;
                        debug!("Loaded conversation index with {} entries", self.index.read().await.len());
                        return Ok(());
                    }
                    Err(err) => {
                        warn!("Corrupted index.json, rebuilding from directories: {err}");
                    }
                },
                Err(err) => {
                    warn!("Failed to read index.json, rebuilding: {err}");
                }
            }
        }

        self.rebuild_index().await
    }

    pub async fn rebuild_index(&self) -> Result<(), ConversationError> {
        let mut map = HashMap::new();
        let mut entries = fs::read_dir(&self.root).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_dir() {
                let conv_file = path.join("conversation.json");
                if conv_file.is_file() {
                    if let Ok(bytes) = fs::read(&conv_file).await {
                        if let Ok(conv) = serde_json::from_slice::<Conversation>(&bytes) {
                            map.insert(conv.id.clone(), conv.to_summary());
                        }
                    }
                }
            }
        }

        *self.index.write().await = map.clone();
        self.persist_index(&map).await?;
        Ok(())
    }

    async fn persist_index(&self, map: &HashMap<String, ConversationSummary>) -> Result<(), ConversationError> {
        let json = serde_json::to_vec_pretty(map)?;
        let index_path = self.index_file();
        let tmp_path = self.root.join(".index.json.tmp");
        fs::write(&tmp_path, json).await?;
        fs::rename(&tmp_path, &index_path).await?;
        Ok(())
    }
}

#[async_trait]
impl ConversationStore for FsConversationStore {
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
        let dir = self.conv_dir(&conversation.id);
        fs::create_dir_all(&dir).await?;

        let file_path = self.conv_file(&conversation.id);
        let tmp_path = dir.join(format!(
            ".conversation.json.tmp.{}.{}",
            std::process::id(),
            crate::types::now_ms()
        ));

        let json = serde_json::to_vec_pretty(conversation)?;
        fs::write(&tmp_path, json).await?;
        fs::rename(&tmp_path, &file_path).await?;

        // Update in-memory index and persist
        let summary = conversation.to_summary();
        let mut lock = self.index.write().await;
        lock.insert(conversation.id.clone(), summary);
        self.persist_index(&lock).await?;

        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Option<Conversation>, ConversationError> {
        let file_path = self.conv_file(id);
        if !file_path.exists() {
            return Ok(None);
        }

        let bytes = match fs::read(&file_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ConversationError::Io(e)),
        };

        let conv: Conversation = serde_json::from_slice(&bytes)?;
        Ok(Some(conv))
    }

    async fn delete(&self, id: &str) -> Result<bool, ConversationError> {
        let dir = self.conv_dir(id);
        if !dir.exists() {
            return Ok(false);
        }

        fs::remove_dir_all(&dir).await?;

        let mut lock = self.index.write().await;
        let removed = lock.remove(id).is_some();
        if removed {
            self.persist_index(&lock).await?;
        }

        Ok(true)
    }

    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError> {
        let lock = self.index.read().await;
        let mut summaries: Vec<ConversationSummary> = lock
            .values()
            .filter(|s| filter.matches(s))
            .cloned()
            .collect();

        // Sort descending by updated_at_ms
        summaries.sort_by(|a, b| b.updated_at_ms.cmp(&a.updated_at_ms));

        if let Some(offset) = filter.offset {
            if offset < summaries.len() {
                summaries = summaries.split_off(offset);
            } else {
                summaries.clear();
            }
        }

        if let Some(limit) = filter.limit {
            summaries.truncate(limit);
        }

        Ok(summaries)
    }

    async fn exists(&self, id: &str) -> Result<bool, ConversationError> {
        let lock = self.index.read().await;
        if lock.contains_key(id) {
            return Ok(true);
        }
        Ok(self.conv_file(id).exists())
    }
}
