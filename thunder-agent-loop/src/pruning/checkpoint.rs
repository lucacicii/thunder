//! Pi-style checkpoint compaction (semantic, LLM-summarized, cache-friendly).
//!
//! Design (mirrors the pi coding agent):
//! - Trigger only near the model's real window limit: `estimated > window - reserve`.
//! - Between two compactions the request prefix stays **byte-identical** — no
//!   incremental rewriting of history (which kept busting provider prompt caches).
//! - On trigger, everything older than `keep_recent_tokens` is summarized by the
//!   LLM into a structured checkpoint (Goal / Constraints / Progress / Decisions /
//!   Next Steps / Critical Context) plus mechanically-extracted file lists.
//! - The checkpoint message persists in the session, so subsequent runs detect it
//!   and summarize *iteratively* (UPDATE-merge with the previous summary).
//! - One-off summarization requests opt out of prompt-cache writes
//!   (`cache_retention = "none"`): they would never be reused.

use crate::core::context::ContextBuffer;
use crate::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use crate::types::config::AgentConfig;
use crate::types::message::ChatMessage;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Stable message name identifying the checkpoint entry inside a context.
pub const CHECKPOINT_MESSAGE_NAME: &str = "context_checkpoint";

/// Max characters a tool result contributes to the summarization input
/// (same bound as pi: keeps the summary request cheap).
const TOOL_RESULT_MAX_CHARS: usize = 2000;
const TOOL_ARGS_MAX_CHARS: usize = 200;

pub const SUMMARIZATION_SYSTEM_PROMPT: &str = "\
You are a context summarization assistant. Your task is to read a conversation \
between a user and an AI assistant, then produce a structured summary following \
the exact format specified.

Do NOT continue the conversation. Do NOT respond to any questions in the \
conversation. ONLY output the structured summary.";

const INITIAL_SUMMARIZATION_PROMPT: &str = "\
The messages above are a conversation to summarize. Create a structured context \
checkpoint summary that another LLM will use to continue the work.

Use this EXACT format:

## Goal
[What is the user trying to accomplish? Can be multiple items if the session covers different tasks.]

## Constraints & Preferences
- [Any constraints, preferences, or requirements mentioned by user]
- [Or \"(none)\" if none were mentioned]

## Progress
### Done
- [x] [Completed tasks/changes]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues preventing progress, if any]

## Key Decisions
- **[Decision]**: [Brief rationale]

## Next Steps
1. [Ordered list of what should happen next]

## Critical Context
- [Any data, examples, or references needed to continue]
- [Or \"(none)\" if not applicable]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

const UPDATE_SUMMARIZATION_PROMPT: &str = "\
The messages above are NEW conversation messages to incorporate into the existing \
summary provided in <previous-summary> tags.

Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from \"In Progress\" to \"Done\" when completed
- UPDATE \"Next Steps\" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
- If something is no longer relevant, you may remove it

Use this EXACT format:

## Goal
[Preserve existing goals, add new ones if the task expanded]

## Constraints & Preferences
- [Preserve existing, add new ones discovered]

## Progress
### Done
- [x] [Include previously done items AND newly completed items]

### In Progress
- [ ] [Current work - update based on progress]

### Blocked
- [Current blockers - remove if resolved]

## Key Decisions
- **[Decision]**: [Brief rationale] (preserve all previous, add new)

## Next Steps
1. [Update based on current state]

## Critical Context
- [Preserve important context, add new if needed]

Keep each section concise. Preserve exact file paths, function names, and error messages.";

// ============================================================================
// Message classification helpers
// ============================================================================

/// Is this entry the persisted checkpoint message?
pub fn is_checkpoint_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::System { name, .. } if name.as_deref() == Some(CHECKPOINT_MESSAGE_NAME))
}

fn is_tool_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::Tool { .. })
}

fn is_user_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::User { .. })
}

// ============================================================================
// Cut point selection
// ============================================================================

/// Lowest index that may be rewritten by compaction: right after the pinned
/// system prompt, or after an existing checkpoint message when present.
pub fn region_floor(buffer: &ContextBuffer) -> usize {
    if buffer
        .get_entry(1)
        .map(|e| is_checkpoint_message(&e.message))
        .unwrap_or(false)
    {
        2
    } else {
        1
    }
}

/// Find the index where the kept (verbatim) region starts.
///
/// Walks backwards from the end accumulating token estimates until
/// `keep_recent_tokens` is covered, then snaps the cut to a safe boundary:
/// - never lands on a tool result (it must stay with its tool call);
/// - prefers the start of the next user message (turn boundary);
/// - never cuts into the pinned system prompt (index 0) or an existing
///   checkpoint message (index 1).
///
/// Returns `None` when there is nothing safe to summarize.
pub fn find_cut_index(buffer: &ContextBuffer, keep_recent_tokens: usize) -> Option<usize> {
    let len = buffer.len();
    if len <= 2 {
        return None;
    }

    // Lowest legal cut: after system prompt + optional existing checkpoint.
    let floor = region_floor(buffer);

    let mut acc = 0usize;
    let mut cut = len;
    for i in (floor..len).rev() {
        if acc >= keep_recent_tokens {
            cut = i + 1;
            break;
        }
        acc += buffer.get_entry(i).map(|e| e.estimated_tokens).unwrap_or(0);
    }
    if cut == len && acc < keep_recent_tokens {
        // Whole history fits inside the keep budget: nothing to summarize.
        return None;
    }
    let mut cut = cut.min(len);

    // Never start the kept region on an orphan tool result: walk the cut
    // backwards past tool messages so each result stays glued to its call.
    while cut > floor && buffer.get_entry(cut).map(|e| is_tool_message(&e.message)).unwrap_or(false) {
        cut -= 1;
    }
    if cut <= floor {
        return None;
    }

    // Prefer cutting exactly at a user-message (turn) boundary: scan forward
    // for the next user message; cutting later only keeps more context.
    for i in cut..len {
        if buffer.get_entry(i).map(|e| is_user_message(&e.message)).unwrap_or(false) {
            return Some(i);
        }
    }

    Some(cut)
}

// ============================================================================
// Serialization for the summarization request
// ============================================================================

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let omitted = text.chars().count() - max;
    format!("{head}\n[... {omitted} chars truncated ...]")
}

/// Serialize a message region into flat text for the summarizer.
/// Prevents the model from treating it as a conversation to continue.
pub fn serialize_region(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            ChatMessage::System { content, .. } => {
                if is_checkpoint_message(msg) {
                    continue; // previous summary is passed separately
                }
                out.push_str(&format!("[System]: {}\n", truncate_chars(content, 2000)));
            }
            ChatMessage::User { content, .. } => {
                out.push_str(&format!("[User]: {}\n", truncate_chars(content, 2000)));
            }
            ChatMessage::Assistant { content, tool_calls, .. } => {
                if let Some(c) = content {
                    if !c.is_empty() {
                        out.push_str(&format!("[Assistant]: {}\n", truncate_chars(c, 2000)));
                    }
                }
                if let Some(calls) = tool_calls {
                    if !calls.is_empty() {
                        let rendered: Vec<String> = calls
                            .iter()
                            .map(|tc| {
                                let args = truncate_chars(&tc.function.arguments, TOOL_ARGS_MAX_CHARS);
                                format!("{}({})", tc.function.name, args)
                            })
                            .collect();
                        out.push_str(&format!("[Assistant tool calls]: {}\n", rendered.join("; ")));
                    }
                }
            }
            ChatMessage::Tool { content, .. } => {
                out.push_str(&format!("[Tool result]: {}\n", truncate_chars(content, TOOL_RESULT_MAX_CHARS)));
            }
        }
    }
    out
}

/// Mechanically extract read/modified file lists from tool calls in the region
/// (write_file → modified; read_file → read-only unless also modified).
pub fn extract_file_lists(messages: &[ChatMessage]) -> (Vec<String>, Vec<String>) {
    let mut read = std::collections::BTreeSet::new();
    let mut modified = std::collections::BTreeSet::new();

    for msg in messages {
        let ChatMessage::Assistant { tool_calls: Some(calls), .. } = msg else {
            continue;
        };
        for tc in calls {
            let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) else {
                continue;
            };
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            match tc.function.name.as_str() {
                "write_file" | "edit_file" => {
                    modified.insert(path.to_string());
                }
                "read_file" => {
                    read.insert(path.to_string());
                }
                _ => {}
            }
        }
    }

    let modified: Vec<String> = modified.into_iter().collect();
    let read_only: Vec<String> = read.into_iter().filter(|p| !modified.contains(p)).collect();
    (read_only, modified)
}

fn format_file_sections(read_files: &[String], modified_files: &[String]) -> String {
    let mut sections = Vec::new();
    if !read_files.is_empty() {
        sections.push(format!("<read-files>\n{}\n</read-files>", read_files.join("\n")));
    }
    if !modified_files.is_empty() {
        sections.push(format!("<modified-files>\n{}\n</modified-files>", modified_files.join("\n")));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

// ============================================================================
// Summarizer
// ============================================================================

/// Performs the one-off LLM summarization call for checkpoint compaction.
pub struct Summarizer {
    client: Arc<dyn LLMClientTrait>,
    model: String,
    max_tokens: usize,
}

impl Summarizer {
    pub fn new(client: Arc<dyn LLMClientTrait>, config: &AgentConfig) -> Self {
        Self {
            client,
            model: config
                .pruning
                .summarizer_model
                .clone()
                .unwrap_or_else(|| config.model.clone()),
            max_tokens: config.pruning.summarizer_max_tokens,
        }
    }

    /// Summarize `serialized_region`, iteratively merging `previous_summary`
    /// when present. Retries transient failures once. A `length` finish or an
    /// empty output is rejected: a partial summary must not become a checkpoint.
    pub async fn summarize(
        &self,
        serialized_region: &str,
        previous_summary: Option<&str>,
        read_files: &[String],
        modified_files: &[String],
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        let prompt = match previous_summary {
            Some(prev) => format!(
                "<previous-summary>\n{prev}\n</previous-summary>\n\n{serialized_region}\n\n{UPDATE_SUMMARIZATION_PROMPT}"
            ),
            None => format!("{serialized_region}\n\n{INITIAL_SUMMARIZATION_PROMPT}"),
        };

        let mut last_err = String::new();
        for _attempt in 0..2 {
            if cancel.is_cancelled() {
                return Err("summarization cancelled".to_string());
            }
            match self.complete(&prompt, cancel).await {
                Ok(text) => {
                    let summary = format!(
                        "[CONTEXT CHECKPOINT — structured summary of earlier conversation]\n\n{text}{}",
                        format_file_sections(read_files, modified_files)
                    );
                    return Ok(summary);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "checkpoint summarization attempt failed");
                    last_err = err;
                }
            }
        }
        Err(last_err)
    }

    async fn complete(&self, prompt: &str, cancel: &CancellationToken) -> Result<String, String> {
        let options = ChatRequestOptions {
            messages: vec![
                ChatMessage::system(SUMMARIZATION_SYSTEM_PROMPT),
                ChatMessage::user(prompt),
            ],
            tools: Vec::new(),
            model: Some(self.model.clone()),
            temperature: None,
            top_p: None,
            max_tokens: Some(self.max_tokens),
            thinking_level: None, // summaries never need deep reasoning
            cache_retention: Some("none".to_string()),
        };

        let mut rx = self.client.stream_chat(options, cancel.clone()).await?;
        let mut content = String::new();
        while let Some(chunk) = rx.recv().await {
            match chunk? {
                LLMStreamChunk::Token(delta) => content.push_str(&delta),
                LLMStreamChunk::Completed { content: final_content, finish_reason, .. } => {
                    if let Some(c) = final_content {
                        if !c.is_empty() {
                            content = c;
                        }
                    }
                    if finish_reason == "length" {
                        return Err("summary generation hit the token cap".to_string());
                    }
                }
                LLMStreamChunk::ReasoningToken(_) | LLMStreamChunk::ToolCallChunk(_) => {}
            }
        }

        if content.trim().is_empty() {
            return Err("summarization produced no content".to_string());
        }
        Ok(content)
    }
}

/// Extract the previous checkpoint (summary text) and where the summarizable
/// region starts, if a checkpoint message exists at index 1.
pub fn previous_checkpoint(buffer: &ContextBuffer) -> Option<(String, usize)> {
    let entry = buffer.get_entry(1)?;
    if is_checkpoint_message(&entry.message) {
        if let ChatMessage::System { content, .. } = &entry.message {
            return Some((content.clone(), 2));
        }
    }
    None
}

/// Build the checkpoint message inserted at index 1 of the rebuilt context.
pub fn checkpoint_message(summary: &str) -> ChatMessage {
    ChatMessage::System {
        content: summary.to_string(),
        name: Some(CHECKPOINT_MESSAGE_NAME.to_string()),
    }
}

/// Token-estimate helper exported for the engine's trigger check.
pub fn checkpoint_trigger(estimated: usize, window: usize, reserve: usize) -> bool {
    estimated > window.saturating_sub(reserve)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_never_lands_on_tool_result() {
        let mut ctx = ContextBuffer::new();
        ctx.set_system_prompt("sys");
        ctx.push(ChatMessage::user("q1"));
        ctx.push(ChatMessage::assistant_text("a1"));
        ctx.push(ChatMessage::assistant(
            None,
            Some(vec![crate::types::message::ToolCall::new_function(
                "t1", "bash", "{}",
            )]),
        ));
        ctx.push(ChatMessage::Tool {
            tool_call_id: "t1".into(),
            content: "out".into(),
            name: Some("bash".into()),
        });
        ctx.push(ChatMessage::user("q2"));
        ctx.push(ChatMessage::assistant_text("a2"));

        // Keep budget covers only the final exchange (q2 + a2): everything
        // older must land in the summarize region.
        let tail: usize = [4usize, 5]
            .iter()
            .filter_map(|i| ctx.get_entry(*i).map(|e| e.estimated_tokens))
            .sum();
        let cut = find_cut_index(&ctx, tail).expect("cut exists");
        let first_kept = ctx.get_entry(cut).unwrap();
        assert!(!is_tool_message(&first_kept.message), "cut landed on a tool result");
        // Snapped forward to a user boundary when one exists.
        assert!(is_user_message(&first_kept.message));
        assert!(cut >= 1);
    }

    #[test]
    fn cut_respects_existing_checkpoint_floor() {
        let mut ctx = ContextBuffer::new();
        ctx.set_system_prompt("sys");
        ctx.push(checkpoint_message("old summary"));
        ctx.push(ChatMessage::user("q1"));
        ctx.push(ChatMessage::assistant_text("a1"));
        ctx.push(ChatMessage::user("q2"));
        ctx.push(ChatMessage::assistant_text("a2"));

        let cut = find_cut_index(&ctx, 1).expect("cut exists");
        assert!(cut >= 2, "cut must not enter the checkpoint/system region");
    }

    #[test]
    fn no_cut_when_history_fits_keep_budget() {
        let mut ctx = ContextBuffer::new();
        ctx.set_system_prompt("sys");
        ctx.push(ChatMessage::user("hi"));
        ctx.push(ChatMessage::assistant_text("hello"));
        assert!(find_cut_index(&ctx, usize::MAX).is_none());
    }

    #[test]
    fn serialization_truncates_tool_results() {
        let long = "x".repeat(5000);
        let text = serialize_region(&[ChatMessage::Tool {
            tool_call_id: "t".into(),
            content: long,
            name: Some("bash".into()),
        }]);
        assert!(text.contains("[Tool result]: "));
        assert!(text.contains("chars truncated"));
        assert!(text.chars().count() < 2400);
    }

    #[test]
    fn file_lists_distinguish_read_and_modified() {
        let msgs = vec![ChatMessage::assistant(
            None,
            Some(vec![
                crate::types::message::ToolCall::new_function(
                    "t1",
                    "read_file",
                    r#"{"path":"a.rs"}"#,
                ),
                crate::types::message::ToolCall::new_function(
                    "t2",
                    "write_file",
                    r#"{"path":"b.rs"}"#,
                ),
                crate::types::message::ToolCall::new_function(
                    "t3",
                    "read_file",
                    r#"{"path":"b.rs"}"#,
                ),
            ]),
        )];
        let (read, modified) = extract_file_lists(&msgs);
        assert_eq!(read, vec!["a.rs".to_string()]);
        assert_eq!(modified, vec!["b.rs".to_string()]);
    }

    #[test]
    fn trigger_math_matches_pi() {
        assert!(!checkpoint_trigger(80, 100, 20));
        assert!(checkpoint_trigger(81, 100, 20));
        // Degenerate config (reserve >= window): the effective budget is 0,
        // mirroring pi's JS arithmetic where the subtraction goes negative —
        // compaction is permanently due.
        assert!(checkpoint_trigger(50, 100, 200));
    }
}
