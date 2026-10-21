#[cfg(feature = "conversation")]
use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use thunder_agent_loop::types::event::{AgentEvent, ObservedEvent, TurnStats};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::AgentRunResult;
use thunder_agent_pack_code::autonomous_system_prompt;
use thunder_conversation::prelude::*;
use tokio::sync::RwLock;

#[cfg(feature = "conversation")]
pub struct ConversationPlugin {
    manifest: PluginManifest,
    store: Arc<dyn ConversationStore>,
    active_conversation: Arc<RwLock<Option<Conversation>>>,
    /// The turn being accumulated. A turn arrives as hundreds of deltas with its
    /// tool results interleaved, and is only whole at its `turn_end`.
    pending_turn: Mutex<Option<PendingTurn>>,
}

/// One engine turn's events, buffered until the turn can be written as a unit.
///
/// The transcript's entry is the turn — "what did the agent do this round" —
/// but nothing in the event stream hands one over ready-made: its text and its
/// chain-of-thought arrive as deltas, and its calls and their results arrive
/// interleaved. Buffering here is what lets a single assistant message carry the
/// whole turn, with that turn's results right after it.
#[cfg(feature = "conversation")]
#[derive(Default)]
struct PendingTurn {
    turn: usize,
    text: String,
    reasoning: String,
    calls: Vec<ToolCall>,
    results: Vec<PendingToolResult>,
}

#[cfg(feature = "conversation")]
struct PendingToolResult {
    tool_call_id: String,
    name: String,
    output: String,
}

/// Take the buffered turn out of its lock, leaving nothing behind.
///
/// Paired with [`put_pending`] so a caller can hold the buffer while it works
/// without holding the guard across an `await`.
#[cfg(feature = "conversation")]
fn take_pending(lock: &Mutex<Option<PendingTurn>>) -> Option<PendingTurn> {
    lock.lock().unwrap_or_else(|e| e.into_inner()).take()
}

#[cfg(feature = "conversation")]
fn put_pending(lock: &Mutex<Option<PendingTurn>>, pending: Option<PendingTurn>) {
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = pending;
}

/// Point the buffer at `turn`, writing out whatever came before it.
///
/// The turn number on the events themselves decides the boundary, not
/// `turn_start`: a run that is observed from partway through still has to split
/// its turns, and a turn that never saw `turn_end` (cancel, daemon error) still
/// has to be written rather than folded into the next one.
#[cfg(feature = "conversation")]
fn begin_turn(conv: &mut Conversation, pending: &mut Option<PendingTurn>, turn: usize) {
    if pending.as_ref().is_some_and(|state| state.turn == turn) {
        return;
    }
    write_turn(conv, pending.take(), None);
    *pending = Some(PendingTurn {
        turn,
        ..PendingTurn::default()
    });
}

/// Write a buffered turn: one assistant message, then the results that answered
/// it. `stats` is absent for a turn whose `turn_end` never arrived.
#[cfg(feature = "conversation")]
fn write_turn(conv: &mut Conversation, pending: Option<PendingTurn>, stats: Option<TurnStats>) {
    let Some(turn) = pending else { return };
    let content = (!turn.text.is_empty()).then_some(turn.text);
    let reasoning = (!turn.reasoning.is_empty()).then_some(turn.reasoning);
    let calls = (!turn.calls.is_empty()).then_some(turn.calls);
    // A turn that produced nothing at all — not even a stat line — is not worth
    // a transcript entry.
    if content.is_none() && reasoning.is_none() && calls.is_none() && stats.is_none() {
        return;
    }
    conv.add_assistant_turn(content, calls, reasoning, stats);
    for result in turn.results {
        conv.add_tool_message(result.tool_call_id, result.output, Some(result.name));
    }
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
        .with_capability(PluginCapability::MemoryPersistence);

        Self {
            manifest,
            store,
            active_conversation: Arc::new(RwLock::new(None)),
            pending_turn: Mutex::new(None),
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
                    .with_system_prompt(autonomous_system_prompt(Default::default()))
            });

            *active = Some(conv);
        }

        // The user's turn is never in the event stream, so seed it from the run
        // context: a conversation with no user turn is not a session and is not
        // persisted at all. Re-running the same prompt on the same session must
        // not duplicate its trailing user turn.
        let prompt = ctx.prompt().map(str::trim).filter(|p| !p.is_empty());
        if let (Some(prompt), Some(conv)) = (prompt, active.as_mut()) {
            let already_recorded = matches!(
                conv.messages.last(),
                Some(ChatMessage::User { content, .. }) if content == prompt
            );
            if !already_recorded {
                conv.add_user_message(prompt);
            }
        }
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, _ctx: &PluginContext) {
        let mut active = self.active_conversation.write().await;
        let Some(conv) = active.as_mut() else { return };

        // The buffer is taken out of its lock for the length of the match and put
        // back after: no guard is held across the checkpoint save below, which a
        // plugin's future (it has to stay `Send`) would not survive.
        let mut pending = take_pending(&self.pending_turn);
        let mut turn_ended = false;
        match &event.event {
            AgentEvent::TurnStart { turn, .. } => {
                begin_turn(conv, &mut pending, *turn);
            }
            AgentEvent::ReasoningDelta { turn, delta } => {
                begin_turn(conv, &mut pending, *turn);
                if let Some(state) = pending.as_mut() {
                    state.reasoning.push_str(delta);
                }
            }
            AgentEvent::TokenDelta { turn, delta } => {
                begin_turn(conv, &mut pending, *turn);
                if let Some(state) = pending.as_mut() {
                    state.text.push_str(delta);
                }
            }
            AgentEvent::ToolCallReady { turn, tool_call } => {
                begin_turn(conv, &mut pending, *turn);
                if let Some(state) = pending.as_mut() {
                    let known = state.calls.iter().any(|call| call.id == tool_call.id);
                    if !known {
                        state.calls.push(tool_call.clone());
                    }
                }
            }
            AgentEvent::ToolExecResult {
                turn,
                tool_call_id,
                name,
                result,
                ..
            } => {
                begin_turn(conv, &mut pending, *turn);
                if let Some(state) = pending.as_mut() {
                    state.results.push(PendingToolResult {
                        tool_call_id: tool_call_id.clone(),
                        name: name.clone(),
                        output: result.output.clone(),
                    });
                }
            }
            AgentEvent::TurnEnd { turn, stats, .. } => {
                // The turn is whole here: its text, its reasoning and its calls
                // become one message, with the results that answered them right
                // after it — the order a provider expects to read back, and the
                // order a transcript wants (one entry per turn, not one per call).
                // A turn whose events never arrived is not written: an empty entry
                // in the transcript is worse than a missing one.
                if pending.as_ref().is_some_and(|state| state.turn == *turn) {
                    write_turn(conv, pending.take(), Some(stats.clone()));
                }
                turn_ended = true;
            }
            _ => {}
        }
        put_pending(&self.pending_turn, pending);

        if turn_ended {
            // Turn-level checkpoint: persist session incrementally to protect against unexpected termination
            let _ = self.store.save(conv).await;
        }
    }

    async fn on_finish(
        &self,
        result: &AgentRunResult,
        _ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        let mut active = self.active_conversation.write().await;
        if let Some(conv) = active.as_mut() {
            // A run that ended without a `turn_end` for its last turn (error,
            // cancel) still has that turn's events; write them before the final
            // text is considered, so nothing the user watched stream is lost.
            let pending = take_pending(&self.pending_turn);
            write_turn(conv, pending, None);

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
