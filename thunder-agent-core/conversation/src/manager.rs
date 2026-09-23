use crate::error::ConversationError;
use crate::exporter::ConversationExporter;
use crate::store::ConversationStore;
use crate::turn::truncate_turns;
use crate::types::{
    now_ms, Conversation, ConversationFilter, ConversationStatus, ConversationSummary,
};
use thunder_agent_loop::AgentRunResult;

#[derive(Debug, Clone)]
pub struct ConversationManager<S: ConversationStore> {
    store: S,
}

impl<S: ConversationStore> ConversationManager<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub async fn create(&self, id: impl Into<String>) -> Result<Conversation, ConversationError> {
        let id_str = id.into();
        if self.store.exists(&id_str).await? {
            return Err(ConversationError::AlreadyExists(id_str));
        }

        let conv = Conversation::new(id_str);
        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn create_with_prompt(
        &self,
        id: impl Into<String>,
        title: Option<String>,
        system_prompt: Option<String>,
    ) -> Result<Conversation, ConversationError> {
        let id_str = id.into();
        if self.store.exists(&id_str).await? {
            return Err(ConversationError::AlreadyExists(id_str));
        }

        let mut conv = Conversation::new(id_str);
        if let Some(t) = title {
            conv = conv.with_title(t);
        }
        if let Some(p) = system_prompt {
            conv = conv.with_system_prompt(p);
        }

        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Conversation>, ConversationError> {
        self.store.load(id).await
    }

    pub async fn get_or_create(&self, id: &str) -> Result<Conversation, ConversationError> {
        match self.store.load(id).await? {
            Some(conv) => Ok(conv),
            None => {
                let conv = Conversation::new(id);
                self.store.save(&conv).await?;
                Ok(conv)
            }
        }
    }

    pub async fn save(&self, conversation: &Conversation) -> Result<(), ConversationError> {
        self.store.save(conversation).await
    }

    pub async fn delete(&self, id: &str) -> Result<bool, ConversationError> {
        self.store.delete(id).await
    }

    pub async fn list(
        &self,
        filter: &ConversationFilter,
    ) -> Result<Vec<ConversationSummary>, ConversationError> {
        self.store.list(filter).await
    }

    pub async fn append_user_message(
        &self,
        id: &str,
        text: impl Into<String>,
    ) -> Result<Conversation, ConversationError> {
        let mut conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;

        conv.add_user_message(text);
        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn append_run_result(
        &self,
        id: &str,
        result: &AgentRunResult,
    ) -> Result<Conversation, ConversationError> {
        let mut conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;

        conv.append_agent_result(result);
        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn append_stage(
        &self,
        id: &str,
        role: impl Into<String>,
        task_brief: impl Into<String>,
        result: &AgentRunResult,
    ) -> Result<Conversation, ConversationError> {
        let mut conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;

        conv.append_stage(role, task_brief, result);
        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn fork(
        &self,
        source_id: &str,
        new_id: impl Into<String>,
        keep_last_n_turns: Option<usize>,
    ) -> Result<Conversation, ConversationError> {
        let new_id_str = new_id.into();
        if self.store.exists(&new_id_str).await? {
            return Err(ConversationError::AlreadyExists(new_id_str));
        }

        let source = self
            .get(source_id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(source_id.to_string()))?;

        let messages = match keep_last_n_turns {
            Some(n) => truncate_turns(&source, n),
            None => source.messages.clone(),
        };

        let now = now_ms();
        let mut branched = Conversation {
            id: new_id_str,
            title: source.title.as_ref().map(|t| format!("{t} (Fork)")),
            parent_id: Some(source.id.clone()),
            system_prompt: source.system_prompt.clone(),
            model: source.model.clone(),
            workspace: source.workspace.clone(),
            thinking_level: source.thinking_level.clone(),
            status: ConversationStatus::Active,
            messages,
            stages: source.stages.clone(),
            orchestration: source.orchestration.clone(),
            metadata: source.metadata.clone(),
            stats: source.stats.clone(),
            created_at_ms: now,
            updated_at_ms: now,
        };

        branched.recalculate_stats();
        self.store.save(&branched).await?;
        Ok(branched)
    }

    pub async fn archive(&self, id: &str) -> Result<Conversation, ConversationError> {
        let mut conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;

        conv.status = ConversationStatus::Archived;
        conv.updated_at_ms = now_ms();
        self.store.save(&conv).await?;
        Ok(conv)
    }

    pub async fn export_markdown(&self, id: &str) -> Result<String, ConversationError> {
        let conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;
        Ok(ConversationExporter::to_markdown(&conv))
    }

    pub async fn export_json(&self, id: &str) -> Result<String, ConversationError> {
        let conv = self
            .get(id)
            .await?
            .ok_or_else(|| ConversationError::NotFound(id.to_string()))?;
        ConversationExporter::to_json(&conv)
    }

    /// Format all saved conversations into a Markdown session catalog for `/resume` command.
    pub async fn format_session_catalog(&self) -> Result<String, ConversationError> {
        let summaries = self.list(&ConversationFilter::new()).await?;
        Ok(Self::format_catalog_markdown(&summaries))
    }

    /// Format conversation summaries into a Markdown catalog table.
    pub fn format_catalog_markdown(summaries: &[ConversationSummary]) -> String {
        if summaries.is_empty() {
            return "No previous conversation sessions found in storage.".to_string();
        }

        let mut out = format!("### 📁 Saved Conversation Sessions (Total: {})\n\n", summaries.len());
        out.push_str("| # | Session ID | Title | Turns | Messages | Last Active |\n");
        out.push_str("|---|---|---|---|---|---|\n");

        for (idx, s) in summaries.iter().enumerate() {
            let title = s.title.as_deref().unwrap_or("Untitled Conversation");
            let clean_title = title.lines().next().unwrap_or("Untitled");
            out.push_str(&format!(
                "| **{}** | `{}` | {} | {} | {} | {} |\n",
                idx + 1,
                s.id,
                clean_title,
                s.turn_count,
                s.message_count,
                s.updated_at_ms
            ));
        }

        out.push_str("\n*Use `/resume <#>` or `/resume <session_id>` to restore and continue any conversation.*");
        out
    }

    /// Resume a conversation by either 1-based index (e.g. "1") or direct session ID.
    pub async fn resume(&self, id_or_index: &str) -> Result<Option<Conversation>, ConversationError> {
        let trimmed = id_or_index.trim();

        // 1. Try parsing as 1-based index from recent list
        if let Ok(idx) = trimmed.parse::<usize>() {
            if idx > 0 {
                let list = self.list(&ConversationFilter::new()).await?;
                if let Some(summary) = list.get(idx - 1) {
                    return self.get(&summary.id).await;
                }
            }
        }

        // 2. Direct ID lookup
        self.get(trimmed).await
    }
}
