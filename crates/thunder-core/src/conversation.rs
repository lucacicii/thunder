use crate::context::ContextBuffer;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_protocol::{thunder_subdir, ChatMessage, ContentPart, Role, ToolCall, TurnStats};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Conversation not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    #[default]
    Active,
    Archived,
    Deleted,
    Paused,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationStats {
    pub total_tokens: usize,
    #[serde(default)]
    pub total_used_tokens: usize,
    pub message_count: usize,
    pub turn_count: usize,
    pub tool_calls_count: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationStatsReport {
    pub session_id: String,
    pub total_messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    pub tool_messages: usize,
    pub estimated_tokens: usize,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrchestrationMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topology: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_routed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage_id: String,
    pub role: String,
    pub agent_id: String,
    pub task_brief: String,
    pub final_content: Option<String>,
    pub turn_count: usize,
    pub tool_calls_count: usize,
    pub duration_ms: u64,
    pub finish_reason: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_roots: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    pub status: ConversationStatus,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<StageRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<OrchestrationMeta>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
    pub stats: ConversationStats,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl Conversation {
    pub fn new(id: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: id.into(),
            title: None,
            title_source: None,
            parent_id: None,
            system_prompt: None,
            model: None,
            workspace: None,
            shared_roots: Vec::new(),
            thinking_level: None,
            status: ConversationStatus::Active,
            messages: Vec::new(),
            stages: Vec::new(),
            orchestration: None,
            metadata: HashMap::new(),
            stats: ConversationStats::default(),
            created_at_ms: now,
            updated_at_ms: now,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn is_title_manual(&self) -> bool {
        self.title_source.as_deref() == Some("manual")
    }

    pub fn is_title_placeholder(&self) -> bool {
        match self.title.as_deref() {
            None => true,
            Some(t) => {
                let t = t.trim();
                t.is_empty()
                    || t.eq_ignore_ascii_case("new conversation")
                    || t.eq_ignore_ascii_case("untitled")
                    || (t.len() >= 3 && t.ends_with("..."))
            }
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        let prompt_str = prompt.into();
        self.system_prompt = Some(prompt_str.clone());
        if self.messages.is_empty() || self.messages[0].role() != Role::System {
            self.messages.insert(0, ChatMessage::system(prompt_str));
        } else {
            self.messages[0] = ChatMessage::system(prompt_str);
        }
        self.recalculate_stats();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    pub fn bind_workspace_if_empty(&mut self, workspace: impl Into<String>) {
        if self.workspace.is_none() {
            self.workspace = Some(workspace.into());
        }
    }

    pub fn with_thinking_level(mut self, level: impl Into<String>) -> Self {
        self.thinking_level = Some(level.into());
        self
    }

    pub fn has_user_turns(&self) -> bool {
        self.messages
            .iter()
            .any(|m| matches!(m, ChatMessage::User { .. }))
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        let content_str = content.into();
        if self.title.is_none() {
            self.title = Some(provisional_title(&content_str));
        }
        self.messages.push(ChatMessage::user(content_str));
        self.touch();
    }

    pub fn add_user_message_with_parts(
        &mut self,
        text: impl Into<String>,
        parts: Vec<ContentPart>,
    ) {
        let text_str = text.into();
        if self.title.is_none() {
            self.title = Some(provisional_title(&text_str));
        }
        self.messages
            .push(ChatMessage::user_multimodal(text_str, parts));
        self.touch();
    }

    pub fn add_assistant_turn(
        &mut self,
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
        reasoning: Option<String>,
        stats: Option<TurnStats>,
    ) {
        self.messages.push(ChatMessage::assistant_turn(
            content, tool_calls, reasoning, stats,
        ));
        self.touch();
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.touch();
    }

    pub fn set_messages(&mut self, messages: Vec<ChatMessage>) {
        self.messages = messages;
        self.touch();
    }

    pub fn to_context_buffer(&self) -> ContextBuffer {
        ContextBuffer::with_messages(self.messages.clone())
    }

    pub fn recalculate_stats(&mut self) {
        let mut total_tokens = 0;
        let mut tool_calls = 0;
        let mut turns = 0;

        for msg in &self.messages {
            total_tokens += ContextBuffer::calculate_message_tokens(msg);
            match msg {
                ChatMessage::User { .. } => turns += 1,
                ChatMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => {
                    tool_calls += calls.len();
                }
                _ => {}
            }
        }

        self.stats.total_tokens = total_tokens;
        self.stats.message_count = self.messages.len();
        self.stats.turn_count = turns;
        self.stats.tool_calls_count = tool_calls;
    }

    pub fn to_summary(&self) -> ConversationSummary {
        let tags = self
            .metadata
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        ConversationSummary {
            id: self.id.clone(),
            title: self.title.clone(),
            parent_id: self.parent_id.clone(),
            model: self.model.clone(),
            workspace: self.workspace.clone(),
            thinking_level: self.thinking_level.clone(),
            status: self.status,
            message_count: self.messages.len(),
            turn_count: self.stats.turn_count,
            total_tokens: self.stats.total_tokens,
            total_used_tokens: self.stats.total_used_tokens,
            tags,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }

    fn touch(&mut self) {
        self.updated_at_ms = now_ms();
        self.recalculate_stats();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    pub status: ConversationStatus,
    pub message_count: usize,
    pub turn_count: usize,
    pub total_tokens: usize,
    #[serde(default)]
    pub total_used_tokens: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ConversationFilter {
    pub status: Option<ConversationStatus>,
    pub parent_id: Option<String>,
    pub keyword: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub min_turns: Option<usize>,
}

impl ConversationFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_min_turns(mut self, min_turns: usize) -> Self {
        self.min_turns = Some(min_turns);
        self
    }

    pub fn matches(&self, summary: &ConversationSummary) -> bool {
        if let Some(min) = self.min_turns {
            if summary.turn_count < min {
                return false;
            }
        }
        if let Some(st) = self.status {
            if summary.status != st {
                return false;
            }
        }
        if let Some(pid) = &self.parent_id {
            if summary.parent_id.as_deref() != Some(pid.as_str()) {
                return false;
            }
        }
        if let Some(kw) = &self.keyword {
            let kw_lower = kw.to_lowercase();
            let matches_title = summary
                .title
                .as_ref()
                .map(|t| t.to_lowercase().contains(&kw_lower))
                .unwrap_or(false);
            let matches_id = summary.id.to_lowercase().contains(&kw_lower);
            let matches_tags = summary
                .tags
                .iter()
                .any(|t| t.to_lowercase().contains(&kw_lower));
            if !matches_title && !matches_id && !matches_tags {
                return false;
            }
        }
        true
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const PROVISIONAL_TITLE_CHARS: usize = 30;

pub fn provisional_title(prompt: &str) -> String {
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > PROVISIONAL_TITLE_CHARS {
        let truncated: String = flat.chars().take(PROVISIONAL_TITLE_CHARS).collect();
        format!("{truncated}...")
    } else {
        flat
    }
}

#[async_trait]
pub trait ConversationStore: Send + Sync {
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError>;
    async fn load(&self, id: &str) -> Result<Option<Conversation>, ConversationError>;
    async fn delete(&self, id: &str) -> Result<bool, ConversationError>;
    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError>;
    async fn exists(&self, id: &str) -> Result<bool, ConversationError>;
}

#[derive(Debug, Clone, Default)]
pub struct MemoryConversationStore {
    conversations: Arc<RwLock<HashMap<String, Conversation>>>,
}

impl MemoryConversationStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ConversationStore for MemoryConversationStore {
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
        if !conversation.has_user_turns() {
            return Ok(());
        }
        self.conversations
            .write()
            .await
            .insert(conversation.id.clone(), conversation.clone());
        Ok(())
    }

    async fn load(&self, id: &str) -> Result<Option<Conversation>, ConversationError> {
        Ok(self.conversations.read().await.get(id).cloned())
    }

    async fn delete(&self, id: &str) -> Result<bool, ConversationError> {
        Ok(self.conversations.write().await.remove(id).is_some())
    }

    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError> {
        let lock = self.conversations.read().await;
        let mut summaries: Vec<ConversationSummary> = lock
            .values()
            .map(|c| c.to_summary())
            .filter(|s| filter.matches(s))
            .collect();
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
        Ok(self.conversations.read().await.contains_key(id))
    }
}

#[derive(Debug, Clone)]
pub struct FsConversationStore {
    root: PathBuf,
    index: Arc<RwLock<HashMap<String, ConversationSummary>>>,
}

impl FsConversationStore {
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, ConversationError> {
        let root = root.into();
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
        thunder_subdir("conversations")
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
            if let Ok(bytes) = fs::read(&index_path).await {
                if let Ok(loaded) =
                    serde_json::from_slice::<HashMap<String, ConversationSummary>>(&bytes)
                {
                    *self.index.write().await = loaded;
                    debug!(
                        "Loaded conversation index with {} entries",
                        self.index.read().await.len()
                    );
                    return Ok(());
                }
            }
        }
        self.rebuild_index().await
    }

    async fn read_disk_index(&self) -> HashMap<String, ConversationSummary> {
        let index_path = self.index_file();
        if !index_path.exists() {
            return HashMap::new();
        }
        match fs::read(&index_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(err) => {
                warn!("Failed to read index.json while merging: {err}");
                HashMap::new()
            }
        }
    }

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

    async fn persist_index(
        &self,
        map: &HashMap<String, ConversationSummary>,
    ) -> Result<(), ConversationError> {
        let mut merged = Self::merge_index_maps(self.read_disk_index().await, map.clone());
        self.prune_missing_dirs(&mut merged).await;
        self.write_index_map(&merged).await
    }

    async fn write_index_map(
        &self,
        map: &HashMap<String, ConversationSummary>,
    ) -> Result<(), ConversationError> {
        let json = serde_json::to_vec_pretty(map)?;
        let index_path = self.index_file();
        let tmp_path = self.root.join(format!(
            ".index.json.tmp.{}.{}",
            std::process::id(),
            now_ms()
        ));
        fs::write(&tmp_path, json).await?;
        fs::rename(&tmp_path, &index_path).await?;
        Ok(())
    }
}

#[async_trait]
impl ConversationStore for FsConversationStore {
    async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
        if !conversation.has_user_turns() {
            return Ok(());
        }
        let dir = self.conv_dir(&conversation.id);
        fs::create_dir_all(&dir).await?;

        let file_path = self.conv_file(&conversation.id);
        let tmp_path = dir.join(format!(
            ".conversation.json.tmp.{}.{}",
            std::process::id(),
            now_ms()
        ));

        let json = serde_json::to_vec_pretty(conversation)?;
        fs::write(&tmp_path, json).await?;
        fs::rename(&tmp_path, &file_path).await?;

        let summary = conversation.to_summary();
        let mut lock = self.index.write().await;
        lock.insert(conversation.id.clone(), summary.clone());
        let mut disk = self.read_disk_index().await;
        if disk.is_empty() && lock.len() > 1 {
            self.persist_index(&lock).await?;
        } else {
            disk.insert(conversation.id.clone(), summary);
            self.write_index_map(&disk).await?;
        }
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
        lock.remove(id);
        let mut disk = self.read_disk_index().await;
        if disk.remove(id).is_some() || !disk.is_empty() {
            self.write_index_map(&disk).await?;
        } else {
            self.persist_index(&lock).await?;
        }
        Ok(true)
    }

    async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError> {
        let memory = self.index.read().await.clone();
        let mut merged = Self::merge_index_maps(self.read_disk_index().await, memory);
        self.prune_missing_dirs(&mut merged).await;

        let mut summaries: Vec<ConversationSummary> = merged
            .values()
            .filter(|s| filter.matches(s))
            .cloned()
            .collect();
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
