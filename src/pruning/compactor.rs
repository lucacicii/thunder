use crate::core::context::ContextBuffer;
use crate::types::message::{ChatMessage, Role};

#[derive(Debug, Clone, Default)]
pub struct CompactionResult {
    pub compacted: bool,
    pub messages_compacted: usize,
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub digest_summary: String,
}

#[derive(Debug, Clone)]
pub struct CompactorConfig {
    /// Token threshold ratio (0.0 to 1.0) relative to max_context_tokens to trigger compaction (default: 0.75)
    pub trigger_ratio: f32,
    /// Minimum number of recent turns to preserve with full high fidelity (default: 3)
    pub preserve_recent_turns: usize,
}

impl Default for CompactorConfig {
    fn default() -> Self {
        Self {
            trigger_ratio: 0.75,
            preserve_recent_turns: 3,
        }
    }
}

pub struct RollingCompactor {
    config: CompactorConfig,
}

impl Default for RollingCompactor {
    fn default() -> Self {
        Self::new(CompactorConfig::default())
    }
}

impl RollingCompactor {
    pub fn new(config: CompactorConfig) -> Self {
        Self { config }
    }

    /// Checks if compaction is needed and performs rolling compaction
    pub fn compact_if_needed(
        &self,
        context: &mut ContextBuffer,
        max_context_tokens: usize,
        artifacts_summary: &str,
    ) -> CompactionResult {
        let tokens_before = context.estimated_tokens();
        let trigger_threshold = (max_context_tokens as f32 * self.config.trigger_ratio) as usize;

        if tokens_before <= trigger_threshold {
            return CompactionResult {
                compacted: false,
                messages_compacted: 0,
                tokens_before,
                tokens_after: tokens_before,
                digest_summary: String::new(),
            };
        }

        self.force_compact(context, artifacts_summary)
    }

    /// Executes rolling compaction on older turns
    pub fn force_compact(&self, context: &mut ContextBuffer, artifacts_summary: &str) -> CompactionResult {
        let tokens_before = context.estimated_tokens();
        let has_pinned_system = !context.is_empty() && context.get_entry(0).map(|e| e.message.role() == Role::System).unwrap_or(false);

        // Check if there is already an existing state digest at index 1
        let has_existing_digest = has_pinned_system
            && context.len() > 1
            && context.get_entry(1).map(|e| match &e.message {
                ChatMessage::System { content, .. } => content.contains("【Previous Conversation Summary"),
                _ => false,
            }).unwrap_or(false);

        let start_idx = if has_existing_digest {
            2
        } else if has_pinned_system {
            1
        } else {
            0
        };

        // Estimate number of messages per turn ~ 3 (User + Assistant + Tool)
        let keep_message_count = self.config.preserve_recent_turns * 3;
        if context.len() <= start_idx + keep_message_count {
            // Not enough older history to meaningfully compact
            return CompactionResult {
                compacted: false,
                messages_compacted: 0,
                tokens_before,
                tokens_after: tokens_before,
                digest_summary: String::new(),
            };
        }

        let cutoff_idx = context.len().saturating_sub(keep_message_count);

        // 1. Extract and summarize facts from the range [start_idx..cutoff_idx]
        let mut key_questions = Vec::new();
        let mut tools_executed = Vec::new();
        let mut key_findings = Vec::new();

        let mut existing_summary_content = String::new();
        if has_existing_digest {
            if let Some(entry) = context.get_entry(1) {
                if let ChatMessage::System { content, .. } = &entry.message {
                    existing_summary_content = content.clone();
                }
            }
        }

        for i in start_idx..cutoff_idx {
            if let Some(entry) = context.get_entry(i) {
                match &entry.message {
                    ChatMessage::User { content, .. } => {
                        let text = content.lines().next().unwrap_or("").trim();
                        if !text.is_empty() && key_questions.len() < 5 {
                            key_questions.push(text.to_string());
                        }
                    }
                    ChatMessage::Assistant { content, tool_calls, .. } => {
                        if let Some(calls) = tool_calls {
                            for c in calls {
                                tools_executed.push(format!("{}({})", c.function.name, c.function.arguments.chars().take(40).collect::<String>()));
                            }
                        }
                        if let Some(c) = content {
                            let text = c.lines().next().unwrap_or("").trim();
                            if !text.is_empty() && key_findings.len() < 5 {
                                key_findings.push(text.to_string());
                            }
                        }
                    }
                    ChatMessage::Tool { name, content, .. } => {
                        if content.contains("Error") || content.contains("failed") {
                            key_findings.push(format!("Tool '{}' output: {}", name.as_deref().unwrap_or("tool"), content.lines().next().unwrap_or("")));
                        }
                    }
                    _ => {}
                }
            }
        }

        // 2. Synthesize High-Density State Digest
        let mut digest = String::from("【Previous Conversation Summary & Milestone State Digest】:\n");

        if !existing_summary_content.is_empty() {
            digest.push_str("• Prior Milestone Context:\n");
            for line in existing_summary_content.lines().take(6) {
                if !line.starts_with('【') {
                    digest.push_str(&format!("  {}\n", line));
                }
            }
        }

        if !key_questions.is_empty() {
            digest.push_str("• Key User Directives Processed:\n");
            for q in &key_questions {
                digest.push_str(&format!("  - {}\n", q));
            }
        }

        if !tools_executed.is_empty() {
            digest.push_str(&format!("• Actions Performed: {} tool executions recorded.\n", tools_executed.len()));
        }

        if !key_findings.is_empty() {
            digest.push_str("• Discovered Facts & Key Progress:\n");
            for f in &key_findings {
                digest.push_str(&format!("  - {}\n", f));
            }
        }

        // Incorporate lossless artifact scratchpad manifest
        if !artifacts_summary.is_empty() {
            digest.push('\n');
            digest.push_str(artifacts_summary);
        }

        // 3. Atomically remove the old message slice [start_idx..cutoff_idx]
        let remove_count = cutoff_idx - start_idx;
        for _ in 0..remove_count {
            context.remove_at(start_idx);
        }

        // 4. In-place insert or replace the State Digest message at index 1 (or 0 if no system prompt)
        let digest_msg = ChatMessage::System {
            content: digest.clone(),
            name: Some("state_digest".to_string()),
        };

        if has_existing_digest {
            // Replace existing digest in place (O(1))
            context.replace_at(1, digest_msg);
        } else if has_pinned_system {
            // Insert at index 1 right after pinned system prompt (O(n) shift, no clone/clear)
            context.insert_at(1, digest_msg);
        } else {
            // Insert at index 0 (top)
            context.insert_at(0, digest_msg);
        }

        let tokens_after = context.estimated_tokens();

        CompactionResult {
            compacted: true,
            messages_compacted: remove_count,
            tokens_before,
            tokens_after,
            digest_summary: digest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rolling_compactor_lifecycle() {
        let mut ctx = ContextBuffer::new();
        ctx.set_system_prompt("Pinned System Prompt");

        // Add 15 turns of conversation
        for i in 1..=15 {
            ctx.push(ChatMessage::user(format!("User step {}", i)));
            ctx.push(ChatMessage::assistant(
                Some(format!("Assistant completed step {}", i)),
                None,
            ));
        }

        let tokens_before = ctx.estimated_tokens();
        assert_eq!(ctx.len(), 31); // 1 system + 15*2 messages

        let compactor = RollingCompactor::new(CompactorConfig {
            trigger_ratio: 0.5,
            preserve_recent_turns: 3,
        });

        let artifacts_summary = "- `.thunder/scratchpad/turn_02_bash.log` (1.5 MB)\n";
        let res = compactor.force_compact(&mut ctx, artifacts_summary);

        assert!(res.compacted);
        assert!(res.tokens_after < tokens_before);

        // Verify layout:
        // index 0: Pinned System Prompt
        // index 1: Milestone State Digest
        // index 2+: Preserved recent turns
        assert_eq!(ctx.get_entry(0).unwrap().message.role(), Role::System);
        let digest_entry = ctx.get_entry(1).unwrap();
        assert!(digest_entry.message.content_str().unwrap().contains("【Previous Conversation Summary"));
        assert!(digest_entry.message.content_str().unwrap().contains("turn_02_bash.log"));
    }
}
