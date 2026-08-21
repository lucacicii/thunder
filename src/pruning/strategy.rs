use crate::core::context::ContextBuffer;
use crate::types::config::{ContextPruningConfig, PruningStrategy};
use crate::types::message::{ChatMessage, Role};

#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub pruned: bool,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub messages_removed: usize,
    pub tool_outputs_truncated: usize,
}

pub struct ContextPruner {
    config: ContextPruningConfig,
}

impl ContextPruner {
    pub fn new(config: ContextPruningConfig) -> Self {
        Self { config }
    }

    pub fn prune(&self, context: &mut ContextBuffer) -> PruneResult {
        let tokens_before = context.estimated_tokens();
        if tokens_before <= self.config.max_context_tokens {
            return PruneResult {
                pruned: false,
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                tool_outputs_truncated: 0,
            };
        }

        let mut tool_outputs_truncated = 0;
        let mut messages_removed = 0;

        if matches!(
            self.config.strategy,
            PruningStrategy::TruncateToolResults | PruningStrategy::Hybrid
        ) {
            tool_outputs_truncated += self.prune_old_tool_results(context);
        }

        if context.estimated_tokens() > self.config.max_context_tokens
            && matches!(
                self.config.strategy,
                PruningStrategy::SlidingWindow | PruningStrategy::Hybrid
            )
        {
            messages_removed += self.prune_sliding_window(context);
        }

        // If STILL over budget in hybrid mode, aggressively truncate even recent tool messages
        if context.estimated_tokens() > self.config.max_context_tokens
            && matches!(self.config.strategy, PruningStrategy::Hybrid)
        {
            tool_outputs_truncated += self.prune_all_large_tool_results(context);
        }

        let tokens_after = context.estimated_tokens();
        PruneResult {
            pruned: tokens_after < tokens_before,
            tokens_before,
            tokens_after,
            messages_removed,
            tool_outputs_truncated,
        }
    }

    fn prune_old_tool_results(&self, context: &mut ContextBuffer) -> usize {
        let mut truncated_count = 0;
        let len = context.len();
        let preserve_boundary = len.saturating_sub(self.config.preserve_last_turns * 4).max(1);

        for i in 1..preserve_boundary {
            if let Some(entry) = context.get_entry(i) {
                if let ChatMessage::Tool {
                    tool_call_id,
                    content,
                    name,
                } = &entry.message
                {
                    if content.len() > 250 {
                        let shortened = format!(
                            "{}\n[... Older tool output trimmed by ContextPruner ...]\n{}",
                            &content[..80.min(content.len())],
                            &content[content.len().saturating_sub(80)..]
                        );
                        let updated = ChatMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            content: shortened,
                            name: name.clone(),
                        };
                        context.replace_at(i, updated);
                        truncated_count += 1;
                    }
                }
            }

            if context.estimated_tokens() <= self.config.max_context_tokens {
                break;
            }
        }

        truncated_count
    }

    fn prune_all_large_tool_results(&self, context: &mut ContextBuffer) -> usize {
        let mut truncated_count = 0;
        for i in 1..context.len() {
            if let Some(entry) = context.get_entry(i) {
                if let ChatMessage::Tool {
                    tool_call_id,
                    content,
                    name,
                } = &entry.message
                {
                    if content.len() > 300 {
                        let shortened = format!(
                            "{}\n[... Tool output compressed for token budget ...]\n{}",
                            &content[..80.min(content.len())],
                            &content[content.len().saturating_sub(80)..]
                        );
                        let updated = ChatMessage::Tool {
                            tool_call_id: tool_call_id.clone(),
                            content: shortened,
                            name: name.clone(),
                        };
                        context.replace_at(i, updated);
                        truncated_count += 1;
                    }
                }
            }
            if context.estimated_tokens() <= self.config.max_context_tokens {
                break;
            }
        }
        truncated_count
    }

    /// Atomic sliding window pruning that never leaves orphaned Tool Calls or Tool Messages
    fn prune_sliding_window(&self, context: &mut ContextBuffer) -> usize {
        let mut removed_count = 0;
        let has_pinned_system = self.config.pin_system_prompt
            && context.get_entry(0).map(|e| e.message.role() == Role::System).unwrap_or(false);

        let start_idx = if has_pinned_system { 1 } else { 0 };
        let min_keep = self.config.preserve_last_turns * 2;

        while context.len() > start_idx + min_keep && context.estimated_tokens() > self.config.max_context_tokens {
            let entry = match context.get_entry(start_idx) {
                Some(e) => e.clone(),
                None => break,
            };

            match &entry.message {
                ChatMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } if !calls.is_empty() => {
                    // Remove the assistant message
                    context.remove_at(start_idx);
                    removed_count += 1;

                    // Atomically remove all matching tool message responses immediately following it
                    let expected_tool_count = calls.len();
                    let mut removed_tools = 0;
                    while removed_tools < expected_tool_count && start_idx < context.len() {
                        if let Some(next_entry) = context.get_entry(start_idx) {
                            if matches!(next_entry.message, ChatMessage::Tool { .. }) {
                                context.remove_at(start_idx);
                                removed_count += 1;
                                removed_tools += 1;
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
                _ => {
                    // Regular User / Assistant message / Orphaned Tool
                    if context.remove_at(start_idx).is_some() {
                        removed_count += 1;
                    } else {
                        break;
                    }
                }
            }
        }

        removed_count
    }
}
