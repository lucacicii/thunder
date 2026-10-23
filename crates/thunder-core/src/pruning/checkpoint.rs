//! Pi-style checkpoint compaction (semantic, LLM-summarized, cache-friendly) ported from `main`.

use crate::context::{ContextBuffer, ContextPruningConfig};
use std::sync::Arc;
use thunder_protocol::{
    ChatMessage, ChatRequestOptions, ContentPart, LLMClientTrait, LLMStreamChunk,
};
use tokio_util::sync::CancellationToken;

pub const CHECKPOINT_MESSAGE_NAME: &str = "context_checkpoint";

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

pub fn is_checkpoint_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::System { name, .. } if name.as_deref() == Some(CHECKPOINT_MESSAGE_NAME))
}

fn is_tool_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::Tool { .. })
}

fn is_user_message(msg: &ChatMessage) -> bool {
    matches!(msg, ChatMessage::User { .. })
}

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

pub fn find_cut_index(buffer: &ContextBuffer, keep_recent_tokens: usize) -> Option<usize> {
    let len = buffer.len();
    if len <= 2 {
        return None;
    }

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
        return None;
    }
    let mut cut = cut.min(len);

    // Never start the kept region on an orphan tool result: walk the cut
    // backwards past tool messages so each result stays glued to its call.
    while cut > floor
        && buffer
            .get_entry(cut)
            .map(|e| is_tool_message(&e.message))
            .unwrap_or(false)
    {
        cut -= 1;
    }
    if cut <= floor {
        return None;
    }

    for i in cut..len {
        if buffer
            .get_entry(i)
            .map(|e| is_user_message(&e.message))
            .unwrap_or(false)
        {
            return Some(i);
        }
    }

    Some(cut)
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    let omitted = text.chars().count() - max;
    format!("{head}\n[... {omitted} chars truncated ...]")
}

pub fn serialize_region(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for msg in messages {
        match msg {
            ChatMessage::System { content, .. } => {
                if is_checkpoint_message(msg) {
                    continue;
                }
                out.push_str(&format!("[System]: {}\n", truncate_chars(content, 2000)));
            }
            ChatMessage::User { content, parts, .. } => {
                out.push_str(&format!("[User]: {}\n", truncate_chars(content, 2000)));
                if let Some(parts) = parts {
                    for part in parts {
                        match part {
                            ContentPart::Text { text } => {
                                out.push_str(&format!("[User]: {}\n", truncate_chars(text, 2000)));
                            }
                            ContentPart::Image {
                                mime_type, data, ..
                            } => {
                                out.push_str(&format!(
                                    "[User attached image: {} (~{} base64 bytes)]\n",
                                    mime_type,
                                    data.len()
                                ));
                            }
                        }
                    }
                }
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
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
                                let args =
                                    truncate_chars(&tc.function.arguments, TOOL_ARGS_MAX_CHARS);
                                format!("{}({})", tc.function.name, args)
                            })
                            .collect();
                        out.push_str(&format!(
                            "[Assistant tool calls]: {}\n",
                            rendered.join("; ")
                        ));
                    }
                }
            }
            ChatMessage::Tool { content, .. } => {
                out.push_str(&format!(
                    "[Tool result]: {}\n",
                    truncate_chars(content, TOOL_RESULT_MAX_CHARS)
                ));
            }
        }
    }
    out
}

/// Last-resort guess for tools the catalog does not know about (a tool removed from the
/// snapshot mid-turn, or a name that never was registered).
///
/// This used to be the primary judgement, which made the kernel sniff tool semantics by
/// name. It is now only the fallback behind [`mutating_oracle`].
pub fn looks_mutating_by_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("write")
        || lower.contains("edit")
        || lower.contains("delete")
        || lower.contains("remove")
        || lower.contains("patch")
        || lower.contains("update")
        || lower.contains("create")
}

/// Build the "does this tool change state?" predicate from a frozen catalog snapshot.
///
/// Plugins declare [`ProgressKind`] once; the pruning layer asks this predicate instead of
/// guessing from the tool name. Unknown names fall back to [`looks_mutating_by_name`].
pub fn mutating_oracle(snapshot: &thunder_registry::CatalogSnapshot) -> impl Fn(&str) -> bool + '_ {
    move |name: &str| match snapshot.descriptors.get(name) {
        Some(descriptor) => descriptor.progress == thunder_protocol::ProgressKind::Mutating,
        None => looks_mutating_by_name(name),
    }
}

/// Split the file paths a conversation touched into (read-only, modified).
///
/// `is_mutating` decides which side a tool belongs to; pass
/// [`mutating_oracle`]`(&registry.current())` in the agent loop.
pub fn extract_file_lists(
    messages: &[ChatMessage],
    is_mutating: &(dyn Fn(&str) -> bool + Send + Sync),
) -> (Vec<String>, Vec<String>) {
    let mut read = std::collections::BTreeSet::new();
    let mut modified = std::collections::BTreeSet::new();

    for msg in messages {
        let ChatMessage::Assistant {
            tool_calls: Some(calls),
            ..
        } = msg
        else {
            continue;
        };
        for tc in calls {
            let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.function.arguments) else {
                continue;
            };
            let Some(path) = args.get("path").and_then(|v| v.as_str()) else {
                continue;
            };
            if is_mutating(&tc.function.name) {
                modified.insert(path.to_string());
            } else {
                read.insert(path.to_string());
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
        sections.push(format!(
            "<read-files>\n{}\n</read-files>",
            read_files.join("\n")
        ));
    }
    if !modified_files.is_empty() {
        sections.push(format!(
            "<modified-files>\n{}\n</modified-files>",
            modified_files.join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!("\n\n{}", sections.join("\n\n"))
    }
}

pub struct Summarizer {
    client: Arc<dyn LLMClientTrait>,
    model: String,
    max_tokens: usize,
    limit: Arc<tokio::sync::Semaphore>,
}

impl Summarizer {
    pub fn new(
        client: Arc<dyn LLMClientTrait>,
        default_model: impl Into<String>,
        pruning: &ContextPruningConfig,
        limit: Arc<tokio::sync::Semaphore>,
    ) -> Self {
        Self {
            client,
            model: pruning
                .summarizer_model
                .clone()
                .unwrap_or_else(|| default_model.into()),
            max_tokens: pruning.summarizer_max_tokens,
            limit,
        }
    }

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

        let _permit = tokio::select! {
            _ = cancel.cancelled() => return Err("summarization cancelled".to_string()),
            permit = self.limit.acquire() => match permit {
                Ok(permit) => permit,
                Err(_) => return Err("summarization limiter closed".to_string()),
            },
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
            thinking_level: None,
            cache_retention: Some("none".to_string()),
            session_id: None,
        };

        let mut rx = self.client.stream_chat(options, cancel.clone()).await?;
        let mut content = String::new();
        while let Some(chunk) = rx.recv().await {
            match chunk? {
                LLMStreamChunk::Token(delta) => content.push_str(&delta),
                LLMStreamChunk::Completed {
                    content: final_content,
                    finish_reason,
                    ..
                } => {
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

pub fn previous_checkpoint(buffer: &ContextBuffer) -> Option<(String, usize)> {
    let entry = buffer.get_entry(1)?;
    if is_checkpoint_message(&entry.message) {
        if let ChatMessage::System { content, .. } = &entry.message {
            return Some((content.clone(), 2));
        }
    }
    None
}

pub fn checkpoint_message(summary: &str) -> ChatMessage {
    ChatMessage::System {
        content: summary.to_string(),
        name: Some(CHECKPOINT_MESSAGE_NAME.to_string()),
    }
}

pub fn checkpoint_trigger(estimated: usize, window: usize, reserve: usize) -> bool {
    estimated > window.saturating_sub(reserve)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thunder_protocol::ToolCall;

    #[test]
    fn cut_never_lands_on_tool_result() {
        let mut ctx = ContextBuffer::new();
        ctx.set_system_prompt("sys");
        ctx.push(ChatMessage::user("q1"));
        ctx.push(ChatMessage::assistant_text("a1"));
        ctx.push(ChatMessage::assistant(
            None,
            Some(vec![ToolCall::new_function("t1", "core__bash", "{}")]),
        ));
        ctx.push(ChatMessage::Tool {
            tool_call_id: "t1".into(),
            content: "out".into(),
            name: Some("core__bash".into()),
        });
        ctx.push(ChatMessage::user("q2"));
        ctx.push(ChatMessage::assistant_text("a2"));

        let tail: usize = [4usize, 5]
            .iter()
            .filter_map(|i| ctx.get_entry(*i).map(|e| e.estimated_tokens))
            .sum();
        let cut = find_cut_index(&ctx, tail).expect("cut exists");
        let first_kept = ctx.get_entry(cut).unwrap();
        assert!(!is_tool_message(&first_kept.message));
        assert!(is_user_message(&first_kept.message));
        assert!(cut >= 1);
    }

    #[test]
    fn file_lists_distinguish_read_and_modified() {
        let msgs = vec![ChatMessage::assistant(
            None,
            Some(vec![
                ToolCall::new_function("t1", "core__read_file", r#"{"path":"a.rs"}"#),
                ToolCall::new_function("t2", "core__write_file", r#"{"path":"b.rs"}"#),
                ToolCall::new_function("t3", "core__read_file", r#"{"path":"b.rs"}"#),
            ]),
        )];
        let (read, modified) = extract_file_lists(&msgs, &looks_mutating_by_name);
        assert_eq!(read, vec!["a.rs".to_string()]);
        assert_eq!(modified, vec!["b.rs".to_string()]);
    }

    #[test]
    fn declared_progress_kind_beats_the_name_heuristic() {
        // A tool named like a reader that actually writes, and vice versa. The declared
        // kind must win: names are not semantics.
        let msgs = vec![ChatMessage::assistant(
            None,
            Some(vec![
                ToolCall::new_function("t1", "core__read_file", r#"{"path":"a.rs"}"#),
                ToolCall::new_function("t2", "core__write_file", r#"{"path":"b.rs"}"#),
            ]),
        )];
        let inverted = |name: &str| name.ends_with("read_file");
        let (read, modified) = extract_file_lists(&msgs, &inverted);
        assert_eq!(read, vec!["b.rs".to_string()], "declared Observing");
        assert_eq!(modified, vec!["a.rs".to_string()], "declared Mutating");
    }

    #[test]
    fn the_oracle_prefers_the_descriptor_and_falls_back_to_names() {
        use thunder_protocol::{ProgressKind, ToolDescriptor};
        use thunder_registry::CatalogSnapshot;

        let mut snapshot = CatalogSnapshot::empty();
        let mut descriptor =
            ToolDescriptor::new("core__sneaky", "d", serde_json::json!({}), "core://sneaky");
        descriptor = descriptor.with_progress(ProgressKind::Mutating);
        snapshot
            .descriptors
            .insert(descriptor.name.clone(), descriptor);

        let oracle = mutating_oracle(&snapshot);
        assert!(
            oracle("core__sneaky"),
            "declared Mutating wins over the name"
        );
        assert!(
            oracle("core__write_file"),
            "unknown name falls back to the name heuristic"
        );
        assert!(!oracle("core__read_file"));
    }
}
