use crate::core::context::ContextBuffer;
use crate::core::utf8::{safe_slice_from, safe_slice_to};
use crate::pruning::compactor::{CompactorConfig, RollingCompactor};
use crate::types::config::{ContextPruningConfig, PruningStrategy};
use crate::types::message::{ChatMessage, Role};

#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub pruned: bool,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub messages_removed: usize,
    pub tool_outputs_truncated: usize,
    pub compacted: bool,
}

pub struct ContextPruner {
    config: ContextPruningConfig,
    compactor: RollingCompactor,
}

impl ContextPruner {
    pub fn new(config: ContextPruningConfig) -> Self {
        let compactor = RollingCompactor::new(CompactorConfig {
            trigger_ratio: 0.80,
            preserve_recent_turns: config.preserve_last_turns.max(3),
        });
        Self { config, compactor }
    }

    pub fn update_max_tokens(&mut self, new_max: usize) {
        self.config.max_context_tokens = new_max;
    }

    pub fn max_tokens(&self) -> usize {
        self.config.max_context_tokens
    }

    pub fn prune(&self, context: &mut ContextBuffer) -> PruneResult {
        self.prune_with_artifacts(context, "")
    }

    /// Full multi-stage context pruning with Rolling Compaction and Scratchpad Artifacts integration
    pub fn prune_with_artifacts(&self, context: &mut ContextBuffer, artifacts_summary: &str) -> PruneResult {
        let tokens_before = context.estimated_tokens();
        if tokens_before <= self.config.max_context_tokens {
            return PruneResult {
                pruned: false,
                tokens_before,
                tokens_after: tokens_before,
                messages_removed: 0,
                tool_outputs_truncated: 0,
                compacted: false,
            };
        }

        let mut tool_outputs_truncated = 0;
        let mut messages_removed = 0;
        let mut was_compacted = false;

        // Stage 1: Rolling Compaction (Synthesizes Milestone State Digest & retains active window)
        if matches!(
            self.config.strategy,
            PruningStrategy::Hybrid
        ) {
            let comp_res = self.compactor.compact_if_needed(context, self.config.max_context_tokens, artifacts_summary);
            if comp_res.compacted {
                was_compacted = true;
                messages_removed += comp_res.messages_compacted;
            }
        }

        // Stage 2: Truncate Older Tool Outputs if still over budget
        if context.estimated_tokens() > self.config.max_context_tokens
            && matches!(
                self.config.strategy,
                PruningStrategy::TruncateToolResults | PruningStrategy::Hybrid
            )
        {
            tool_outputs_truncated += self.prune_old_tool_results(context);
        }

        // Stage 3: Atomic Sliding Window (Preserving Pinned System Prompt & State Digest)
        if context.estimated_tokens() > self.config.max_context_tokens
            && matches!(
                self.config.strategy,
                PruningStrategy::SlidingWindow | PruningStrategy::Hybrid
            )
        {
            messages_removed += self.prune_sliding_window(context);
        }

        // Stage 4: Aggressive Tool Output Compression for emergency budget enforcement
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
            compacted: was_compacted,
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
                        // UTF-8 safe slicing (prevents panics on multi-byte characters)
                        let shortened = format!(
                            "{}\n[... Older tool output trimmed by ContextPruner ...]\n{}",
                            safe_slice_to(content, 80),
                            safe_slice_from(content, content.len().saturating_sub(80))
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
                        // UTF-8 safe slicing (prevents panics on multi-byte characters)
                        let shortened = format!(
                            "{}\n[... Tool output compressed for token budget ...]\n{}",
                            safe_slice_to(content, 80),
                            safe_slice_from(content, content.len().saturating_sub(80))
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

    /// Atomic sliding window pruning that never leaves orphaned Tool Calls or Tool Messages,
    /// and preserves both the Pinned System Prompt and Milestone State Digest.
    fn prune_sliding_window(&self, context: &mut ContextBuffer) -> usize {
        let mut removed_count = 0;
        let has_pinned_system = self.config.pin_system_prompt
            && context.get_entry(0).map(|e| e.message.role() == Role::System).unwrap_or(false);

        let has_state_digest = has_pinned_system
            && context.len() > 1
            && context.get_entry(1).map(|e| match &e.message {
                ChatMessage::System { content, .. } => content.contains("【Previous Conversation Summary"),
                _ => false,
            }).unwrap_or(false);

        let start_idx = if has_state_digest {
            2
        } else if has_pinned_system {
            1
        } else {
            0
        };

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
