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
        // `create_dir_all` followed by `read_dir` is not atomic: when several
        // processes (the TUI and the daemon), or parallel test tasks, build a
        // store for the same *fresh* root at the same moment, a loser can
        // observe a transient NotFound between another caller's `mkdir` and
        // its completion. Retry the whole sequence a few times instead of
        // failing every caller that lost the race.
        let mut last_err: Option<std::io::Error> = None;
        for _ in 0..10 {
            match Self::open_ready(root.clone()).await {
                Ok(store) => return Ok(store),
                Err(ConversationError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    last_err = Some(err);
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(err) => return Err(err),
            }
        }
        Err(ConversationError::Io(last_err.unwrap_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "conversation store root is unavailable",
            )
        })))
    }

    /// One attempt at `new`; the caller retries transient NotFound races.
    async fn open_ready(root: PathBuf) -> Result<Self, ConversationError> {
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

    fn raw_transcript_file(&self, id: &str) -> PathBuf {
        self.conv_dir(id).join("raw_transcript.jsonl")
    }

    /// Persist the raw, pre-compaction transcript as one JSON object per line.
    ///
    /// Pi-style lifecycle: the working context may be replaced by a checkpoint
    /// summary (the projection), but the original history is never silently
    /// destroyed. Atomic overwrite (tmp + rename) keeps this idempotent —
    /// callers may pass the full snapshot on every checkpoint.
    pub async fn save_raw_transcript(
        &self,
        session_id: &str,
        messages: &[thunder_agent_loop::types::message::ChatMessage],
    ) -> Result<PathBuf, ConversationError> {
        let dir = self.conv_dir(session_id);
        fs::create_dir_all(&dir).await?;

        let mut body = String::new();
        for msg in messages {
            body.push_str(&serde_json::to_string(msg)?);
            body.push('\n');
        }

        let file_path = self.raw_transcript_file(session_id);
        let tmp_path = dir.join(format!(
            ".raw_transcript.jsonl.tmp.{}.{}",
            std::process::id(),
            crate::types::now_ms()
        ));
        fs::write(&tmp_path, body).await?;
        fs::rename(&tmp_path, &file_path).await?;
        Ok(file_path)
    }

    /// Load a previously persisted raw transcript, if any.
    pub async fn load_raw_transcript(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<thunder_agent_loop::types::message::ChatMessage>>, ConversationError>
    {
        let path = self.raw_transcript_file(session_id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(ConversationError::Io(e)),
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut messages = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::from_str(line)?);
        }
        Ok(Some(messages))
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
                Ok(bytes) => {
                    match serde_json::from_slice::<HashMap<String, ConversationSummary>>(&bytes) {
                        Ok(loaded) => {
                            *self.index.write().await = loaded;
                            debug!(
                                "Loaded conversation index with {} entries",
                                self.index.read().await.len()
                            );
                            return Ok(());
                        }
                        Err(err) => {
                            warn!("Corrupted index.json, rebuilding from directories: {err}");
                        }
                    }
                }
                Err(err) => {
                    warn!("Failed to read index.json, rebuilding: {err}");
                }
            }
        }

        self.rebuild_index().await
    }

    /// Read the on-disk index, tolerating absence and corruption.
    ///
    /// The store root is shared between processes (the TUI and the daemon
    /// both default to `~/.thunder/conversations`), so the on-disk index can
    /// hold rows this process has never seen. A corrupt or unreadable file
    /// degrades to "no rows" rather than an error: callers merge on top of
    /// it and the rebuild path remains available.
    async fn read_disk_index(&self) -> HashMap<String, ConversationSummary> {
        let index_path = self.index_file();
        if !index_path.exists() {
            return HashMap::new();
        }
        match fs::read(&index_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|err| {
                warn!("Corrupted index.json while merging: {err}");
                HashMap::new()
            }),
            Err(err) => {
                warn!("Failed to read index.json while merging: {err}");
                HashMap::new()
            }
        }
    }

    /// Drop index rows whose conversation directory no longer exists.
    ///
    /// A delete issued by *another* process removes the directory first; a
    /// stale in-memory copy here must not resurrect that row on the next
    /// write. An inconclusive stat keeps the row — never destroy an entry
    /// because of a transient filesystem error.
    async fn prune_missing_dirs(&self, map: &mut HashMap<String, ConversationSummary>) {
        let mut vanished = Vec::new();
        for id in map.keys() {
            let conv_file = self.conv_file(id);
            if let Ok(false) = tokio::fs::try_exists(&conv_file).await {
                vanished.push(id.clone());
            }
        }
        for id in vanished {
            map.remove(&id);
        }
    }

    /// Merge `overlay` into `base`, preferring the row with the newer
    /// `updated_at_ms` on id conflicts.
    ///
    /// Owned maps because the merge is done by moving entries; the callers
    /// only need the result.
    fn merge_index_maps(
        mut base: HashMap<String, ConversationSummary>,
        overlay: HashMap<String, ConversationSummary>,
    ) -> HashMap<String, ConversationSummary> {
        for (id, summary) in overlay {
            match base.get(&id) {
                Some(existing) if existing.updated_at_ms > summary.updated_at_ms => {}
                _ => {
                    base.insert(id, summary);
                }
            }
        }
        base
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

    /// Persist the index with a read-merge-write cycle.
    ///
    /// Writing the in-memory map verbatim would erase rows created by other
    /// processes sharing this root (the classic TUI + daemon collision: the
    /// later writer's stale full-map overwrite makes the other process's new
    /// conversations invisible to `list`). Instead: re-read the on-disk index,
    /// merge our rows in (newer `updated_at_ms` wins on conflicts), prune rows
    /// whose directories have vanished, then atomically rename. The tmp file
    /// is unique per write so concurrent writers never clobber each other's
    /// in-flight temp file.
    async fn persist_index(
        &self,
        map: &HashMap<String, ConversationSummary>,
    ) -> Result<(), ConversationError> {
        let mut merged = Self::merge_index_maps(self.read_disk_index().await, map.clone());
        self.prune_missing_dirs(&mut merged).await;

        let json = serde_json::to_vec_pretty(&merged)?;
        let index_path = self.index_file();
        let tmp_path = self.root.join(format!(
            ".index.json.tmp.{}.{}",
            std::process::id(),
            crate::types::now_ms()
        ));
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
        // Merge the in-memory view with the on-disk index: conversations
        // created by another process sharing this root (TUI while the daemon
        // runs, or vice versa) must be visible without a restart. Newer rows
        // win; rows without a backing directory are dropped so deletes made
        // elsewhere are respected here too. One small file read per call —
        // `list` is a panel-facing path, not a hot loop.
        let memory = self.index.read().await.clone();
        let mut merged = Self::merge_index_maps(self.read_disk_index().await, memory);
        self.prune_missing_dirs(&mut merged).await;

        let mut summaries: Vec<ConversationSummary> = merged
            .values()
            .filter(|s| filter.matches(s))
            .cloned()
            .collect();

        // Sort descending by updated_at_ms
        summaries.sort_by_key(|a| std::cmp::Reverse(a.updated_at_ms));

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
