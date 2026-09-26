//! Context pruning: pi-style checkpoint compaction with an emergency floor.
//!
//! There is exactly **one** compaction strategy: swap older history for an
//! LLM-generated structured checkpoint when the model's real window is
//! approached (see [`crate::pruning::checkpoint`]).
//!
//! A single internal safety net remains: [`ContextPruner::emergency_trim`],
//! which drops the oldest complete turns when a checkpoint cannot be produced
//! (summarizer unavailable/failed) *and* the context is over the hard window.
//! It exists only so an unattended run can never wedge itself; it is **not** a
//! configurable strategy and never runs while checkpointing works.

use crate::core::context::ContextBuffer;
use crate::pruning::checkpoint::{
    self, find_cut_index, previous_checkpoint, region_floor, Summarizer,
};
use crate::types::config::ContextPruningConfig;
use crate::types::message::ChatMessage;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default)]
pub struct PruneResult {
    pub pruned: bool,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub messages_removed: usize,
    /// A checkpoint summary was generated and swapped in (pi-style compaction).
    pub checkpoint: bool,
    /// The emergency trim ran because summarization was unavailable/failed.
    pub emergency: bool,
}

pub struct ContextPruner {
    config: ContextPruningConfig,
}

impl ContextPruner {
    pub fn new(config: ContextPruningConfig) -> Self {
        Self { config }
    }

    pub fn update_max_tokens(&mut self, new_max: usize) {
        self.config.max_context_tokens = new_max;
    }

    pub fn max_tokens(&self) -> usize {
        self.config.max_context_tokens
    }

    /// Does the context cross the compaction trigger?
    /// `estimated > max_context_tokens - reserve_tokens`.
    pub fn should_compact(&self, estimated_tokens: usize) -> bool {
        checkpoint::checkpoint_trigger(
            estimated_tokens,
            self.config.max_context_tokens,
            self.config.reserve_tokens,
        )
    }

    /// Run compaction for this turn.
    ///
    /// No-op unless the window trigger is crossed — which is what keeps the
    /// request prefix byte-stable (and the provider prompt cache hot) between
    /// compactions.
    pub async fn prune(
        &self,
        context: &mut ContextBuffer,
        summarizer: Option<&Summarizer>,
        cancel: &CancellationToken,
    ) -> PruneResult {
        let tokens_before = context.estimated_tokens();
        if !self.should_compact(tokens_before) {
            return PruneResult {
                tokens_before,
                tokens_after: tokens_before,
                ..Default::default()
            };
        }

        let cut = find_cut_index(context, self.config.keep_recent_tokens);
        let previous = previous_checkpoint(context);
        let region_start = previous
            .as_ref()
            .map(|(_, start)| *start)
            .unwrap_or_else(|| region_floor(context));

        // Summarize the older region when there is one and a summarizer is
        // available; otherwise fall through to the emergency trim.
        if let (Some(cut), Some(sum)) = (cut, summarizer) {
            if cut > region_start {
                let region: Vec<ChatMessage> = (region_start..cut)
                    .filter_map(|i| context.get_entry(i).map(|e| e.message.clone()))
                    .collect();

                let serialized = checkpoint::serialize_region(&region);
                let (read_files, modified_files) = checkpoint::extract_file_lists(&region);
                let prev = previous
                    .as_ref()
                    .map(|(text, _)| text.as_str())
                    .filter(|s| !s.is_empty());

                match sum
                    .summarize(&serialized, prev, &read_files, &modified_files, cancel)
                    .await
                {
                    Ok(summary) => {
                        // One-time, bounded rewrite: the new prefix is stable
                        // until the next checkpoint.
                        for _ in (region_start..cut).rev() {
                            context.remove_at(region_start);
                        }
                        let msg = checkpoint::checkpoint_message(&summary);
                        if previous.is_some() {
                            // Iterative compaction: replace the prior checkpoint.
                            context.replace_at(1, msg);
                        } else {
                            context.insert_at(region_start, msg);
                        }

                        let tokens_after = context.estimated_tokens();
                        if tokens_after <= self.config.max_context_tokens {
                            return PruneResult {
                                pruned: true,
                                checkpoint: true,
                                tokens_before,
                                tokens_after,
                                messages_removed: cut - region_start,
                                emergency: false,
                            };
                        }
                        // Still over the hard window (huge kept tail):
                        // fall through to the emergency trim below.
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "checkpoint summarization failed; using emergency trim"
                        );
                    }
                }
            }
        }

        if context.estimated_tokens() <= self.config.max_context_tokens {
            // Compaction could not shrink (e.g. nothing safe to summarize, or
            // the summarizer produced nothing) but we are under the hard limit:
            // report honestly instead of destroying history.
            let tokens_after = context.estimated_tokens();
            return PruneResult {
                pruned: tokens_after < tokens_before,
                tokens_before,
                tokens_after,
                ..Default::default()
            };
        }

        let removed = self.emergency_trim(context);
        let tokens_after = context.estimated_tokens();
        PruneResult {
            pruned: removed > 0,
            emergency: true,
            tokens_before,
            tokens_after,
            messages_removed: removed,
            checkpoint: false,
        }
    }

    /// Last-resort hard-window protection: drop the oldest complete turns.
    ///
    /// Atomic with respect to tool pairing: a tool result is never separated
    /// from its tool call. Used only when checkpointing is unavailable and the
    /// context exceeds the model's hard window, so this never fights the cache
    /// in the normal path.
    pub fn emergency_trim(&self, context: &mut ContextBuffer) -> usize {
        let floor = region_floor(context);
        let mut removed = 0usize;
        while context.estimated_tokens() > self.config.max_context_tokens {
            let Some(cut) = find_cut_index(context, self.config.keep_recent_tokens) else {
                break;
            };
            if cut <= floor {
                break;
            }
            let before = context.len();
            for _ in floor..cut {
                context.remove_at(floor);
            }
            removed += before - context.len();
        }
        removed
    }
}
