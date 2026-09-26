#[cfg(feature = "conversation")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use std::sync::Arc;
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent};
use thunder_agent_loop::types::message::ChatMessage;
use thunder_agent_loop::AgentRunResult;
use thunder_conversation::prelude::*;
use tokio::sync::RwLock;

#[cfg(feature = "conversation")]
pub struct ConversationPlugin {
    manifest: PluginManifest,
    store: Arc<dyn ConversationStore>,
    active_conversation: Arc<RwLock<Option<Conversation>>>,
}

#[cfg(feature = "conversation")]
impl ConversationPlugin {
    pub fn new(store: Arc<dyn ConversationStore>) -> Self {
        let manifest = PluginManifest::new(
            "conversation",
            "Thunder Conversation & Session Manager",
            "Manages multi-turn conversation context, session history, and atomic file-system persistence.",
            "0.1.0",
        )
        .with_capability(PluginCapability::MemoryPersistence)
        // Baseline plugin: always active. Session history semantics must not
        // flicker per message — a changing system prompt / toolset busts the
        // provider prompt cache for the whole request prefix.
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            store,
            active_conversation: Arc::new(RwLock::new(None)),
        }
    }

    pub fn with_memory_store() -> Self {
        Self::new(Arc::new(MemoryConversationStore::new()))
    }

    pub async fn with_fs_store() -> Result<Self, ConversationError> {
        let store = FsConversationStore::new(FsConversationStore::default_store_root()).await?;
        Ok(Self::new(Arc::new(store)))
    }

    pub fn active_conversation(&self) -> Arc<RwLock<Option<Conversation>>> {
        self.active_conversation.clone()
    }

    pub fn store(&self) -> Arc<dyn ConversationStore> {
        self.store.clone()
    }
}

#[cfg(feature = "conversation")]
#[async_trait]
impl ThunderPlugin for ConversationPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn system_prompt_contribution(&self) -> Option<String> {
        Some("Session history and multi-turn state are tracked automatically.".to_string())
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let mut active = self.active_conversation.write().await;
        if active.is_none()
            || active
                .as_ref()
                .map(|c| c.id != ctx.session_id)
                .unwrap_or(false)
        {
            // Load or initialize conversation for this session
            let loaded = self
                .store
                .load(&ctx.session_id)
                .await
                .map_err(|e| PluginError::InitFailed(e.to_string()))?;

            let conv = loaded.unwrap_or_else(|| {
                Conversation::new(&ctx.session_id)
                    .with_title("Thunder Root Session")
                    .with_system_prompt(
                        "You are a helpful, fast, autonomous AI engineering assistant.",
                    )
            });

            *active = Some(conv);
        }
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, _ctx: &PluginContext) {
        let mut active = self.active_conversation.write().await;
        if let Some(conv) = active.as_mut() {
            match &event.event {
                AgentEvent::ToolCallReady { tool_call, .. } => {
                    let call_id = tool_call.id.clone();
                    let already_committed = conv.messages.iter().rev().any(|message| {
                        matches!(
                            message,
                            ChatMessage::Assistant {
                                tool_calls: Some(calls),
                                ..
                            } if calls.iter().any(|call| call.id == call_id)
                        )
                    });

                    if !already_committed {
                        conv.add_assistant_message(None, Some(vec![tool_call.clone()]));
                    }
                }
                AgentEvent::ToolExecResult {
                    tool_call_id,
                    name,
                    result,
                    ..
                } => {
                    conv.add_tool_message(tool_call_id, result.output.clone(), Some(name.clone()));
                }
                AgentEvent::TurnEnd { .. } => {
                    // Turn-level checkpoint: persist session incrementally to protect against unexpected termination
                    let _ = self.store.save(conv).await;
                }
                _ => {}
            }
        }
    }

    async fn on_finish(
        &self,
        result: &AgentRunResult,
        _ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        let mut active = self.active_conversation.write().await;
        if let Some(conv) = active.as_mut() {
            if let Some(final_text) = &result.final_content {
                let already_present = conv
                    .messages
                    .last()
                    .map(|m| match m {
                        ChatMessage::Assistant {
                            content: Some(c), ..
                        } => c == final_text,
                        _ => false,
                    })
                    .unwrap_or(false);

                if !already_present && !final_text.is_empty() {
                    conv.add_assistant_message(Some(final_text.clone()), None);
                }
            }

            self.store
                .save(conv)
                .await
                .map_err(|e| PluginError::ExecutionFailed(e.to_string()))?;
        }
        Ok(())
    }
}
