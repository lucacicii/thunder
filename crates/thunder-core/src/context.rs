use crate::token_estimator::{estimate_image_tokens, estimate_token_count};
use serde::{Deserialize, Serialize};
use thunder_protocol::{ChatMessage, ContentPart, Role};

/// Configuration for pi-style checkpoint compaction and context window management.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPruningConfig {
    pub max_context_tokens: usize,
    pub reserve_tokens: usize,
    pub keep_recent_tokens: usize,
    pub summarizer_model: Option<String>,
    pub summarizer_max_tokens: usize,
}

impl Default for ContextPruningConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            summarizer_model: None,
            summarizer_max_tokens: 4_096,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageEntry {
    pub message: ChatMessage,
    pub estimated_tokens: usize,
}

/// Placeholder body used when a `tool_call` never produced a real result.
pub const ABORTED_TOOL_RESULT_PLACEHOLDER: &str =
    "[aborted: this tool call was not executed because the run ended early.]";

/// Repair `tool_calls` / tool-message pairing in a message list before it is
/// replayed to a provider.
///
/// Providers require every assistant `tool_calls` entry to be followed by a tool
/// message carrying the matching id; a single unpaired entry makes the whole
/// conversation unusable (HTTP 400). Conversations persisted by older builds can
/// already be in that state, so the agent loop repairs on load. Orphaned tool
/// messages (no matching `tool_call`) are dropped for the same reason.
///
/// Returns the repaired list plus the number of messages inserted or dropped; a
/// non-zero count on load means the persisted conversation was corrupt.
pub fn repair_tool_call_pairing(messages: Vec<ChatMessage>) -> (Vec<ChatMessage>, usize) {
    use std::collections::VecDeque;

    fn flush(
        out: &mut Vec<ChatMessage>,
        pending: &mut VecDeque<(String, Option<String>)>,
        repaired: &mut usize,
    ) {
        while let Some((id, name)) = pending.pop_front() {
            out.push(ChatMessage::tool(id, ABORTED_TOOL_RESULT_PLACEHOLDER, name));
            *repaired += 1;
        }
    }

    let mut out = Vec::with_capacity(messages.len());
    let mut pending: VecDeque<(String, Option<String>)> = VecDeque::new();
    let mut repaired = 0usize;

    for msg in messages {
        match &msg {
            ChatMessage::Assistant {
                tool_calls: Some(calls),
                ..
            } => {
                flush(&mut out, &mut pending, &mut repaired);
                pending = calls
                    .iter()
                    .map(|c| (c.id.clone(), Some(c.function.name.clone())))
                    .collect();
                out.push(msg);
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                if pending.iter().any(|(id, _)| id == tool_call_id) {
                    // Results must stay in request order: fill any skipped ones first.
                    while pending
                        .front()
                        .map(|(id, _)| id != tool_call_id)
                        .unwrap_or(false)
                    {
                        let (id, name) = pending.pop_front().expect("front checked above");
                        out.push(ChatMessage::tool(id, ABORTED_TOOL_RESULT_PLACEHOLDER, name));
                        repaired += 1;
                    }
                    pending.pop_front();
                    out.push(msg);
                } else {
                    // Orphaned tool result: no `tool_call` asked for it.
                    repaired += 1;
                }
            }
            _ => {
                flush(&mut out, &mut pending, &mut repaired);
                out.push(msg);
            }
        }
    }
    flush(&mut out, &mut pending, &mut repaired);
    (out, repaired)
}

/// Token-accounting message buffer preserving exact message prefixes between compactions.
#[derive(Debug, Clone, Default)]
pub struct ContextBuffer {
    entries: Vec<MessageEntry>,
    total_estimated_tokens: usize,
}

impl ContextBuffer {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_estimated_tokens: 0,
        }
    }

    pub fn with_system_prompt(system_prompt: impl Into<String>) -> Self {
        let mut buf = Self::new();
        let sys = system_prompt.into();
        if !sys.trim().is_empty() {
            buf.set_system_prompt(sys);
        }
        buf
    }

    pub fn with_messages(messages: Vec<ChatMessage>) -> Self {
        let mut ctx = Self::new();
        for msg in messages {
            ctx.push(msg);
        }
        ctx
    }

    pub fn calculate_message_tokens(msg: &ChatMessage) -> usize {
        let mut tokens = 4; // Envelope overhead
        match msg {
            ChatMessage::System { content, name } => {
                if name.is_some() {
                    tokens += 1;
                }
                tokens += estimate_token_count(content);
            }
            ChatMessage::User {
                content,
                name,
                parts,
            } => {
                if name.is_some() {
                    tokens += 1;
                }
                tokens += estimate_token_count(content);
                if let Some(parts) = parts {
                    for part in parts {
                        match part {
                            ContentPart::Text { text } => {
                                tokens += estimate_token_count(text);
                            }
                            ContentPart::Image { mime_type, .. } => {
                                tokens += estimate_image_tokens(mime_type);
                            }
                        }
                    }
                }
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                name,
                ..
            } => {
                if name.is_some() {
                    tokens += 1;
                }
                if let Some(c) = content {
                    tokens += estimate_token_count(c);
                }
                if let Some(calls) = tool_calls {
                    for tc in calls {
                        tokens += 8;
                        tokens += estimate_token_count(&tc.function.name);
                        tokens += estimate_token_count(&tc.function.arguments);
                    }
                }
            }
            ChatMessage::Tool { content, name, .. } => {
                if name.is_some() {
                    tokens += 1;
                }
                tokens += estimate_token_count(content);
            }
        }
        tokens
    }

    pub fn push(&mut self, message: ChatMessage) {
        let tokens = Self::calculate_message_tokens(&message);
        self.total_estimated_tokens += tokens;
        self.entries.push(MessageEntry {
            message,
            estimated_tokens: tokens,
        });
    }

    /// Inject a `<system_notification>` Registry Update block at turn start (§4.3).
    pub fn inject_system_notification(&mut self, notification_xml: String) {
        self.push(ChatMessage::user(notification_xml));
    }

    pub fn insert_at(&mut self, index: usize, message: ChatMessage) {
        let tokens = Self::calculate_message_tokens(&message);
        self.total_estimated_tokens += tokens;
        let clamped_idx = index.min(self.entries.len());
        self.entries.insert(
            clamped_idx,
            MessageEntry {
                message,
                estimated_tokens: tokens,
            },
        );
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn estimated_tokens(&self) -> usize {
        self.total_estimated_tokens
    }

    pub fn get_messages(&self) -> Vec<ChatMessage> {
        self.entries.iter().map(|e| e.message.clone()).collect()
    }

    pub fn get_entry(&self, index: usize) -> Option<&MessageEntry> {
        self.entries.get(index)
    }

    pub fn set_system_prompt(&mut self, prompt: impl Into<String>) {
        let sys_msg = ChatMessage::system(prompt);
        let tokens = Self::calculate_message_tokens(&sys_msg);

        if !self.entries.is_empty() && self.entries[0].message.role() == Role::System {
            let old_tokens = self.entries[0].estimated_tokens;
            self.total_estimated_tokens =
                self.total_estimated_tokens.saturating_sub(old_tokens) + tokens;
            self.entries[0] = MessageEntry {
                message: sys_msg,
                estimated_tokens: tokens,
            };
        } else {
            self.total_estimated_tokens += tokens;
            self.entries.insert(
                0,
                MessageEntry {
                    message: sys_msg,
                    estimated_tokens: tokens,
                },
            );
        }
    }

    pub fn replace_at(&mut self, index: usize, new_message: ChatMessage) {
        if let Some(entry) = self.entries.get_mut(index) {
            let new_tokens = Self::calculate_message_tokens(&new_message);
            self.total_estimated_tokens = self
                .total_estimated_tokens
                .saturating_sub(entry.estimated_tokens)
                + new_tokens;
            *entry = MessageEntry {
                message: new_message,
                estimated_tokens: new_tokens,
            };
        }
    }

    pub fn remove_at(&mut self, index: usize) -> Option<ChatMessage> {
        if index < self.entries.len() {
            let removed = self.entries.remove(index);
            self.total_estimated_tokens = self
                .total_estimated_tokens
                .saturating_sub(removed.estimated_tokens);
            Some(removed.message)
        } else {
            None
        }
    }

    pub fn pop(&mut self) -> Option<ChatMessage> {
        if !self.entries.is_empty() {
            self.remove_at(self.entries.len() - 1)
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_estimated_tokens = 0;
    }
}

#[cfg(test)]
mod pairing_tests {
    use super::*;
    use serde_json::json;
    use thunder_protocol::ToolCall;

    fn call(id: &str) -> ToolCall {
        ToolCall::new_function(id, "mcp__test__echo", json!({ "n": 1 }).to_string())
    }

    fn assistant_with(ids: &[&str]) -> ChatMessage {
        ChatMessage::assistant_turn(
            None,
            Some(ids.iter().map(|id| call(id)).collect()),
            None,
            None,
        )
    }

    fn tool_ids(messages: &[ChatMessage]) -> Vec<String> {
        messages
            .iter()
            .filter_map(|m| match m {
                ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn fills_tool_calls_that_never_ran() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant_with(&["a", "b", "c"]),
            ChatMessage::tool("a", "ok", None),
        ];
        let (repaired, count) = repair_tool_call_pairing(messages);
        assert_eq!(count, 2);
        assert_eq!(tool_ids(&repaired), vec!["a", "b", "c"]);
    }

    #[test]
    fn drops_orphaned_tool_results() {
        let messages = vec![
            ChatMessage::user("go"),
            ChatMessage::tool("ghost", "stale", None),
            ChatMessage::assistant_text("done"),
        ];
        let (repaired, count) = repair_tool_call_pairing(messages);
        assert_eq!(count, 1);
        assert!(tool_ids(&repaired).is_empty());
        assert_eq!(repaired.len(), 2);
    }

    #[test]
    fn keeps_matching_pairs_untouched() {
        let messages = vec![
            ChatMessage::user("go"),
            assistant_with(&["a", "b"]),
            ChatMessage::tool("a", "ok", None),
            ChatMessage::tool("b", "ok", None),
            ChatMessage::assistant_text("done"),
        ];
        let before = messages.len();
        let (repaired, count) = repair_tool_call_pairing(messages);
        assert_eq!(count, 0, "a balanced payload must not be rewritten");
        assert_eq!(repaired.len(), before);
        assert_eq!(tool_ids(&repaired), vec!["a", "b"]);
    }

    #[test]
    fn reorders_placeholders_to_preserve_request_order() {
        // `a` never produced a result, `b` did: the placeholder for `a` has to
        // come first or providers reject the out-of-order result.
        let messages = vec![
            ChatMessage::user("go"),
            assistant_with(&["a", "b"]),
            ChatMessage::tool("b", "ok", None),
        ];
        let (repaired, count) = repair_tool_call_pairing(messages);
        assert_eq!(count, 1);
        assert_eq!(tool_ids(&repaired), vec!["a", "b"]);
        let contents: Vec<&str> = repaired
            .iter()
            .filter_map(|m| match m {
                ChatMessage::Tool { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents[0], ABORTED_TOOL_RESULT_PLACEHOLDER);
        assert_eq!(contents[1], "ok");
    }
}
