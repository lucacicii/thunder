use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::types::message::{ChatMessage, Role, ToolCall};
use thunder_agent_loop::AgentRunResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ConversationStatus {
    #[default]
    Active,
    Archived,
    Deleted,
    Paused,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationStats {
    /// Estimated size of the current working context, in tokens. Recalculated
    /// from the message list on every change, so it tracks the *present* context
    /// window rather than lifetime usage.
    pub total_tokens: usize,
    /// Cumulative provider-reported usage across every task in this conversation,
    /// including cache reads/writes (i.e. the billed prompt volume, not just the
    /// working-context estimate). Never recomputed from message text.
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
    /// Origin of the current title: "auto" (LLM generated) or "manual" (user set).
    /// Manual titles are never overwritten by auto-naming unless forced.
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
    /// Extra roots granted the same read/write standing as the workspace
    /// (e.g. repositories referenced by the task). Merged across runs.
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

    /// True when the current title was explicitly set by the user
    pub fn is_title_manual(&self) -> bool {
        self.title_source.as_deref() == Some("manual")
    }

    /// True when the title is missing or still the initial truncated-prompt placeholder
    pub fn is_title_placeholder(&self) -> bool {
        match self.title.as_deref() {
            None => true,
            Some(t) => {
                let t = t.trim();
                t.is_empty() || (t.len() >= 3 && t.ends_with("..."))
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

    pub fn with_parent_id(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
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

    pub fn with_orchestration(mut self, meta: OrchestrationMeta) -> Self {
        self.orchestration = Some(meta);
        self
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        let content_str = content.into();
        if self.title.is_none() {
            let auto_title: String = content_str.chars().take(40).collect();
            self.title = Some(auto_title);
        }
        self.messages.push(ChatMessage::user(content_str));
        self.touch();
    }

    pub fn add_assistant_message(
        &mut self,
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
    ) {
        self.messages
            .push(ChatMessage::assistant(content, tool_calls));
        self.touch();
    }

    pub fn add_tool_message(
        &mut self,
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        name: Option<String>,
    ) {
        self.messages
            .push(ChatMessage::tool(tool_call_id, content, name));
        self.touch();
    }

    pub fn add_message(&mut self, message: ChatMessage) {
        self.messages.push(message);
        self.touch();
    }

    pub fn append_agent_result(&mut self, result: &AgentRunResult) {
        // Exclude initial system message if conversation already has one
        let has_system = !self.messages.is_empty() && self.messages[0].role() == Role::System;
        for (idx, msg) in result.messages.iter().enumerate() {
            if idx == 0 && msg.role() == Role::System && has_system {
                continue;
            }
            self.messages.push(msg.clone());
        }
        self.stats.duration_ms += result.stats.total_duration_ms;
        self.touch();
    }

    pub fn append_stage(
        &mut self,
        role: impl Into<String>,
        task_brief: impl Into<String>,
        result: &AgentRunResult,
    ) {
        let role_str = role.into();
        let brief_str = task_brief.into();
        let stage_idx = self.stages.len() + 1;
        let stage = StageRecord {
            stage_id: format!("stage_{stage_idx}_{role_str}"),
            role: role_str,
            agent_id: result.agent_id.clone(),
            task_brief: brief_str,
            final_content: result.final_content.clone(),
            turn_count: result.stats.total_turns,
            tool_calls_count: result.stats.total_tool_executions,
            duration_ms: result.stats.total_duration_ms,
            finish_reason: format!("{:?}", result.finish_reason),
            timestamp_ms: now_ms(),
        };

        self.stages.push(stage);
        self.append_agent_result(result);
    }

    pub fn to_context_buffer(&self) -> ContextBuffer {
        ContextBuffer::with_messages(self.messages.clone())
    }

    pub fn to_messages(&self) -> Vec<ChatMessage> {
        self.messages.clone()
    }

    pub fn last_message(&self) -> Option<&ChatMessage> {
        self.messages.last()
    }

    pub fn last_assistant_content(&self) -> Option<&str> {
        self.messages.iter().rev().find_map(|m| match m {
            ChatMessage::Assistant {
                content: Some(c), ..
            } => Some(c.as_str()),
            _ => None,
        })
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

    /// Compact conversation history by keeping the initial system prompt, summarizing
    /// older turns into a single summary message, and retaining the last `keep_recent_messages`.
    /// Returns (count_before, count_after).
    pub fn compact_history(&mut self, keep_recent_messages: usize) -> (usize, usize) {
        let count_before = self.messages.len();
        if count_before <= keep_recent_messages + 1 {
            return (count_before, count_before);
        }

        let sys_prompt = self.system_prompt.clone().unwrap_or_else(|| {
            "You are a helpful, fast, autonomous AI engineering assistant.".to_string()
        });

        let recent_slice: Vec<_> = self
            .messages
            .iter()
            .rev()
            .take(keep_recent_messages)
            .cloned()
            .collect();

        let mut new_msgs = vec![
            ChatMessage::system(sys_prompt),
            ChatMessage::system("Summary of earlier conversation: dialogue turns discussed system tasks, code reviews, skills and execution instructions."),
        ];

        for m in recent_slice.into_iter().rev() {
            new_msgs.push(m);
        }

        self.messages = new_msgs;
        self.touch();
        (count_before, self.messages.len())
    }

    /// Generate statistical usage and token metrics for this conversation.
    pub fn generate_stats_report(&self) -> ConversationStatsReport {
        let user_count = self
            .messages
            .iter()
            .filter(|m| m.role() == Role::User)
            .count();
        let asst_count = self
            .messages
            .iter()
            .filter(|m| m.role() == Role::Assistant)
            .count();
        let tool_count = self
            .messages
            .iter()
            .filter(|m| m.role() == Role::Tool)
            .count();

        let mut char_count = 0;
        for m in &self.messages {
            match m {
                ChatMessage::User { content, .. } => char_count += content.len(),
                ChatMessage::Assistant { content, .. } => {
                    char_count += content.as_ref().map(|c| c.len()).unwrap_or(0)
                }
                ChatMessage::Tool { content, .. } => char_count += content.len(),
                ChatMessage::System { content, .. } => char_count += content.len(),
            }
        }

        let est_tokens = char_count / 3;

        ConversationStatsReport {
            session_id: self.id.clone(),
            total_messages: self.messages.len(),
            user_messages: user_count,
            assistant_messages: asst_count,
            tool_messages: tool_count,
            estimated_tokens: est_tokens,
            total_duration_ms: self.stats.duration_ms,
        }
    }

    /// Format the statistical report into a Markdown dashboard.
    pub fn format_stats_markdown(&self) -> String {
        let stats = self.generate_stats_report();
        let mut out = format!(
            "### 📊 Session Statistics (Session: `{}`)\n\n",
            stats.session_id
        );
        out.push_str(&format!("- **Total Messages**: {}\n", stats.total_messages));
        out.push_str(&format!("- **User Prompts**: {}\n", stats.user_messages));
        out.push_str(&format!(
            "- **Assistant Responses**: {}\n",
            stats.assistant_messages
        ));
        out.push_str(&format!(
            "- **Tool Call Invocations**: {}\n",
            stats.tool_messages
        ));
        out.push_str(&format!(
            "- **Estimated Context Tokens**: ~{} tokens\n",
            stats.estimated_tokens
        ));
        if stats.total_duration_ms > 0 {
            out.push_str(&format!(
                "- **Execution Time**: {}ms\n",
                stats.total_duration_ms
            ));
        }
        out
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
}

impl ConversationFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_status(mut self, status: ConversationStatus) -> Self {
        self.status = Some(status);
        self
    }

    pub fn with_parent_id(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    pub fn with_keyword(mut self, kw: impl Into<String>) -> Self {
        self.keyword = Some(kw.into());
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    pub fn matches(&self, summary: &ConversationSummary) -> bool {
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
