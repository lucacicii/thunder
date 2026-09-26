use crate::core::token_estimator::estimate_token_count;
use crate::types::message::{ChatMessage, Role};

#[derive(Debug, Clone)]
pub struct MessageEntry {
    pub message: ChatMessage,
    pub estimated_tokens: usize,
}

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
            ChatMessage::User { content, name } => {
                if name.is_some() {
                    tokens += 1;
                }
                tokens += estimate_token_count(content);
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
                        tokens += 8; // Tool call base overhead
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
