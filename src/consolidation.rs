//! The idle deep-consolidation tier.
//!
//! The small model writes one memory per work episode, every few minutes, and
//! its `ADD` vs `UPDATE` decision only looks at the last 15 minutes. Over a day
//! that leaves the table full of fragments that overlap each other — and one
//! memory that kept absorbing episodes ends up as a stack of `[Progress Update]`
//! paragraphs rather than a fact.
//!
//! This tier is the other end of the same idea: when the desktop has been quiet
//! for a while, a large model reads the day's fragments, merges the ones that
//! describe the same work, and rewrites them as fewer, denser memories. Sources
//! are marked `superseded` so retrieval stops citing them twice; their capture
//! ids are carried onto the consolidated memory, so nothing is lost from the
//! trail.
//!
//! It runs on a *different* model from the instant tier by design: merging is
//! where a 0.5B model's output would just be more fragments.

use crate::config::MemoryConfig;
use crate::db::{Database, LongTermMemoryRow, NewLongTermMemory};
use crate::error::MemoryError;
use crate::safe_truncate_str;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

/// How much of one memory's summary the prompt may carry.
const MAX_SUMMARY_BYTES: usize = 400;

/// Cap on the whole entry block, so a week of long memories cannot turn one
/// merge into a prompt the model spends minutes thinking about.
const MAX_ENTRY_BYTES: usize = 8_000;

/// Bound the answer. A reasoning model left uncapped will happily think for
/// several minutes about a three-line input; a pass that cannot finish is worse
/// than a pass that answers tersely, because the sources stay active and the
/// next idle window retries them anyway.
///
/// Generous on purpose: this model spends most of the budget on its own
/// reasoning (measured ~550 completion tokens to answer a one-line prompt), and
/// cutting it off mid-thought returns an empty `content` — which parses as a
/// failure and leaves the group for the next pass.
const MAX_OUTPUT_TOKENS: u32 = 16_000;

/// Upper bound on memories handed to one model call. The window is a day or a
/// week of fragments; sending all of them would be one enormous prompt whose
/// answer is harder to trust than several small ones.
const MAX_GROUP_SIZE: usize = 40;

/// Cap on the capture-id trail carried onto a consolidated memory.
const MAX_SOURCE_CAPTURES: usize = 200;

/// What one deep pass did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsolidationReport {
    /// Memories read from the window
    pub candidates: usize,
    /// Categories that had more than one memory to merge
    pub groups: usize,
    /// Source memories folded away
    pub superseded: usize,
    /// Consolidated memories written
    pub produced: usize,
    /// Nothing was written (a dry run, or the model answered unusably)
    pub dry_run: bool,
}

impl ConsolidationReport {
    pub fn is_empty(&self) -> bool {
        self.produced == 0 && self.superseded == 0
    }
}

/// One memory as the model returns it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsolidatedMemory {
    pub category: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub key_entities: Vec<String>,
    #[serde(default)]
    pub importance_score: f32,
    /// Ids of the source memories this one absorbed (filled by us, not the model)
    #[serde(default)]
    pub source_memory_ids: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct ConsolidationResponse {
    memories: Vec<ConsolidatedMemory>,
}

pub struct ConsolidationEngine {
    db: Database,
    config: MemoryConfig,
    http_client: reqwest::Client,
    /// One pass at a time: a second tick while the first is still waiting on the
    /// model would read the same fragments and write a duplicate.
    running: AtomicBool,
}

impl ConsolidationEngine {
    pub fn new(db: Database, config: MemoryConfig) -> Self {
        let timeout = std::time::Duration::from_secs(config.consolidation_timeout_secs.max(10));
        Self {
            db,
            config,
            http_client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default(),
            running: AtomicBool::new(false),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.consolidation_endpoint.is_some()
    }

    /// Take the run slot, or report that a pass is already in flight.
    fn try_begin(&self) -> bool {
        !self.running.swap(true, Ordering::AcqRel)
    }

    fn end(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// Read the window, merge each category, rewrite the table.
    pub async fn run(&self, dry_run: bool) -> Result<ConsolidationReport, MemoryError> {
        let Some(endpoint) = self.config.consolidation_endpoint.clone() else {
            debug!("No `consolidation_endpoint` configured, the deep tier is off");
            return Ok(ConsolidationReport {
                dry_run,
                ..Default::default()
            });
        };
        if !self.try_begin() {
            debug!("A deep consolidation pass is already running");
            return Ok(ConsolidationReport {
                dry_run,
                ..Default::default()
            });
        }

        let result = self.run_inner(&endpoint, dry_run).await;
        self.end();
        result
    }

    async fn run_inner(
        &self,
        endpoint: &str,
        dry_run: bool,
    ) -> Result<ConsolidationReport, MemoryError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let window_start_ms = now_ms - (self.config.consolidation_window_days as i64) * 86_400_000;
        let candidates = self
            .db
            .list_active_memories_since(window_start_ms, 500)?;

        let mut report = ConsolidationReport {
            candidates: candidates.len(),
            dry_run,
            ..Default::default()
        };
        if candidates.is_empty() {
            debug!("No active long-term memories in the consolidation window");
            return Ok(report);
        }

        for (category, group) in Self::group_by_category(candidates) {
            // A single memory is already as consolidated as it can get; asking a
            // model to rewrite it would only reword it.
            if group.len() < 2 {
                continue;
            }
            report.groups += 1;

            let merged = match self.merge_group(endpoint, &category, &group).await {
                Ok(memories) => memories,
                Err(e) => {
                    warn!("Deep consolidation of '{category}' failed: {e}");
                    continue;
                }
            };

            let source_ids: Vec<i64> = group.iter().map(|m| m.id).collect();
            let mut produced_in_group = 0usize;
            for memory in merged {
                // The trail must survive the merge, but repeating thousands of
                // capture ids across records only bloats the row — dedupe, keep
                // the most recent ones.
                let mut source_captures: Vec<i64> = group
                    .iter()
                    .flat_map(|m| m.source_capture_ids.iter().copied())
                    .collect();
                source_captures.sort_unstable();
                source_captures.dedup();
                if source_captures.len() > MAX_SOURCE_CAPTURES {
                    source_captures = source_captures.split_off(source_captures.len() - MAX_SOURCE_CAPTURES);
                }
                let new_memory = NewLongTermMemory {
                    category: memory.category.clone(),
                    title: memory.title.clone(),
                    summary: memory.summary.clone(),
                    key_entities: memory.key_entities.clone(),
                    importance_score: memory.importance_score,
                    time_window_start_ms: group
                        .iter()
                        .map(|m| m.time_window_start_ms)
                        .min()
                        .unwrap_or(now_ms),
                    time_window_end_ms: group
                        .iter()
                        .map(|m| m.time_window_end_ms)
                        .max()
                        .unwrap_or(now_ms),
                    source_capture_ids: source_captures,
                };
                if dry_run {
                    info!(
                        "[dry run] would write '{}' absorbing {} memories",
                        new_memory.title,
                        source_ids.len()
                    );
                } else {
                    let id = self.db.insert_long_term_memory(&new_memory)?;
                    info!(
                        "Consolidated memory #{id} '{}' absorbs {} memories",
                        new_memory.title,
                        source_ids.len()
                    );
                }
                produced_in_group += 1;
                report.produced += 1;
            }

            // Only a group that actually produced something may retire its
            // sources, otherwise a model answer we could not use would delete
            // memories nothing replaced.
            if !dry_run && produced_in_group > 0 {
                report.superseded += self.db.supersede_memories(&source_ids)?;
            }
        }

        Ok(report)
    }

    /// Group by category, biggest groups first, each capped at `MAX_GROUP_SIZE`.
    fn group_by_category(
        memories: Vec<LongTermMemoryRow>,
    ) -> Vec<(String, Vec<LongTermMemoryRow>)> {
        let mut groups: Vec<(String, Vec<LongTermMemoryRow>)> = Vec::new();
        for memory in memories {
            match groups.iter_mut().find(|(cat, _)| cat == &memory.category) {
                Some((_, group)) => {
                    if group.len() < MAX_GROUP_SIZE {
                        group.push(memory);
                    }
                }
                None => groups.push((memory.category.clone(), vec![memory])),
            }
        }
        groups.sort_by_key(|(_, group)| std::cmp::Reverse(group.len()));
        groups
    }

    async fn merge_group(
        &self,
        endpoint: &str,
        category: &str,
        group: &[LongTermMemoryRow],
    ) -> Result<Vec<ConsolidatedMemory>, MemoryError> {
        let prompt = Self::build_prompt(category, group);
        let url = if endpoint.ends_with("/chat/completions") {
            endpoint.to_string()
        } else {
            format!("{}/chat/completions", endpoint.trim_end_matches('/'))
        };

        let mut req = self.http_client.post(&url).json(&serde_json::json!({
            "model": &self.config.consolidation_model,
            "messages": [
                {"role": "system", "content": "You consolidate a developer's desktop activity memory. You merge overlapping episodes into fewer, denser records. Output only valid JSON."},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.1,
            "max_tokens": MAX_OUTPUT_TOKENS,
            "response_format": {"type": "json_object"}
        }));
        if let Some(ref key) = self.config.consolidation_api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let request = req.build()?;
        let retry_request = request.try_clone();

        // One retry, and only for a transport failure. A remote model behind
        // whatever the network does today drops a request now and then (this
        // machine's traffic goes through a transparent proxy); a group is worth
        // a second attempt before waiting hours for the next idle window.
        let resp = match self.http_client.execute(request).await {
            Ok(resp) => resp,
            Err(first_err) => {
                let Some(retry) = retry_request else {
                    return Err(first_err.into());
                };
                debug!("Consolidation request failed ({first_err}), retrying once");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                self.http_client.execute(retry).await?
            }
        };
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::Synthesis(format!(
                "HTTP {status} from consolidation endpoint: {}",
                safe_truncate_str(&body, 300)
            )));
        }

        let json_body: serde_json::Value = resp.json().await?;
        let content = json_body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| MemoryError::Synthesis("Invalid consolidation response".to_string()))?;

        let mut memories = Self::parse_response(content)?;
        // The model may re-categorize; keep whatever it said, but never let a
        // blank one through — `category` drives the retrieval weights.
        for memory in &mut memories {
            if memory.category.trim().is_empty() {
                memory.category = category.to_string();
            }
            memory.importance_score = Self::normalize_importance(memory.importance_score);
        }
        memories.retain(|m| !m.title.trim().is_empty());
        Ok(memories)
    }

    fn build_prompt(category: &str, group: &[LongTermMemoryRow]) -> String {
        let mut entries: Vec<String> = Vec::new();
        let mut used_bytes = 0usize;

        for (idx, m) in group.iter().enumerate() {
            let entry = format!(
                "{}. [{}] {} | entities: {} | importance: {:.1}\n   {}",
                idx + 1,
                m.category,
                m.title,
                m.key_entities.join(", "),
                m.importance_score,
                Self::sanitize(&m.summary.replace('\n', " "))
            );
            // Stop at the budget instead of truncating mid-entry: a memory that
            // appears half-written is worse input than one that is absent.
            if used_bytes + entry.len() > MAX_ENTRY_BYTES && !entries.is_empty() {
                break;
            }
            used_bytes += entry.len();
            entries.push(entry);
        }

        format!(
            r#"These are {count} memories recorded from one developer's desktop activity, all tagged `{category}`, in chronological order. Overlapping ones describe the same work: one may be an update appended to another, or two may be the same task seen at different times.

Merge them into a single set of records — no more than {max_out} — where each record states a piece of work once, in full, and nothing is lost that a future reader would need.

Rules:
- Merge only what truly describes the same work. Unrelated projects stay separate records.
- Never invent facts: keep file paths, tools, decisions and problems exactly as recorded.
- Drop noise: repeated "[Progress Update]" restatements, duplicated context, OCR garbage.
- Preserve outcomes: what was decided, what broke, what was fixed.

Respond ONLY with a valid JSON object:
{{
  "memories": [
    {{
      "category": "{category}",
      "title": "<concise title, under 12 words>",
      "summary": "<2-4 sentences: what was being done, key decisions, outcome>",
      "key_entities": ["<file, tool, package or symbol>"],
      "importance_score": <1.0 to 10.0>
    }}
  ]
}}

Memories:
{entries}"#,
            count = group.len(),
            max_out = (group.len() / 2).max(1),
            category = category,
            entries = entries.join("\n")
        )
    }

    /// Strip what OCR and a small model leave in a summary: control characters,
    /// replacement characters, and runs of whitespace. Prompts leave this machine
    /// now, so this is both a robustness and a politeness measure.
    fn sanitize(text: &str) -> String {
        let cleaned: String = text
            .chars()
            .filter(|c| *c != '\u{FFFD}' && (*c == '\n' || *c == '\t' || !c.is_control()))
            .collect();
        let bytes = safe_truncate_str(&cleaned, MAX_SUMMARY_BYTES);
        bytes.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Parse the model's answer.
    ///
    /// Strict on shape, tolerant on the one thing models get wrong constantly:
    /// they wrap JSON in prose or a fence. Both are stripped before parsing, so
    /// a chatty answer does not silently become a heuristic fallback.
    pub fn parse_response(content: &str) -> Result<Vec<ConsolidatedMemory>, MemoryError> {
        let trimmed = content.trim();
        let unfenced = trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(trimmed)
            .trim();

        // Some models still prepend a sentence; take the outermost object.
        let json_slice = match (unfenced.find('{'), unfenced.rfind('}')) {
            (Some(start), Some(end)) if end > start => &unfenced[start..=end],
            _ => unfenced,
        };

        let parsed: ConsolidationResponse = serde_json::from_str(json_slice).map_err(|e| {
            MemoryError::Synthesis(format!(
                "Failed to parse consolidation JSON: {e} (raw: {})",
                safe_truncate_str(json_slice, 300)
            ))
        })?;
        Ok(parsed.memories)
    }

    /// Put a model's importance score on the table's 1..10 scale.
    ///
    /// Asked for 1..10, models hand back 0..1 often enough that treating 0.8 as
    /// "almost the least important memory that can exist" would quietly bury
    /// exactly the work worth keeping.
    fn normalize_importance(score: f32) -> f32 {
        let scaled = if score > 0.0 && score <= 1.0 {
            score * 10.0
        } else {
            score
        };
        scaled.clamp(1.0, 10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(id: i64, category: &str, title: &str) -> LongTermMemoryRow {
        LongTermMemoryRow {
            id,
            created_at_ms: id * 1000,
            updated_at_ms: id * 1000,
            category: category.to_string(),
            title: title.to_string(),
            summary: format!("summary {id}"),
            key_entities: vec![format!("entity{id}")],
            importance_score: 5.0,
            time_window_start_ms: id * 1000,
            time_window_end_ms: id * 1000,
            source_capture_ids: vec![id * 10],
            status: "active".to_string(),
            utility_score: 1.0,
            access_count: 0,
            last_accessed_at_ms: 0,
        }
    }

    #[test]
    fn groups_by_category_and_puts_the_richest_first() {
        let groups = ConsolidationEngine::group_by_category(vec![
            memory(1, "debugging", "a"),
            memory(2, "code_development", "b"),
            memory(3, "code_development", "c"),
            memory(4, "code_development", "d"),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "code_development");
        assert_eq!(groups[0].1.len(), 3);
        assert_eq!(groups[1].0, "debugging");
    }

    #[test]
    fn caps_a_group_at_the_prompt_budget() {
        let memories = (0..MAX_GROUP_SIZE as i64 + 12)
            .map(|i| memory(i, "coding", "t"))
            .collect();
        let groups = ConsolidationEngine::group_by_category(memories);
        assert_eq!(groups[0].1.len(), MAX_GROUP_SIZE);
    }

    #[test]
    fn parses_a_plain_json_answer() {
        let memories = ConsolidationEngine::parse_response(
            r#"{"memories":[{"category":"debugging","title":"Fixed build","summary":"s","key_entities":["a.rs"],"importance_score":7.5}]}"#,
        )
        .unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].title, "Fixed build");
    }

    #[test]
    fn parses_an_answer_wrapped_in_a_fence_or_prose() {
        let fenced = "```json\n{\"memories\":[{\"category\":\"c\",\"title\":\"t\",\"summary\":\"s\",\"importance_score\":5}]}\n```";
        assert_eq!(ConsolidationEngine::parse_response(fenced).unwrap().len(), 1);

        let chatty = "Sure! Here is the JSON:\n{\"memories\":[{\"category\":\"c\",\"title\":\"t\",\"summary\":\"s\",\"importance_score\":5}]}\nHope that helps.";
        assert_eq!(ConsolidationEngine::parse_response(chatty).unwrap().len(), 1);
    }

    #[test]
    fn rejects_an_answer_without_the_memories_key() {
        assert!(ConsolidationEngine::parse_response("{\"items\":[]}").is_err());
    }

    #[test]
    fn rescales_a_zero_to_one_importance_score() {
        // Measured: a 30B reasoning model asked for 1..10 answers 0.78.
        assert!((ConsolidationEngine::normalize_importance(0.78) - 7.8).abs() < 0.01);
        assert_eq!(ConsolidationEngine::normalize_importance(7.5), 7.5);
        assert_eq!(ConsolidationEngine::normalize_importance(0.0), 1.0);
        assert_eq!(ConsolidationEngine::normalize_importance(42.0), 10.0);
    }

    #[test]
    fn prompt_carries_every_memory_once() {
        let group = vec![memory(1, "coding", "first"), memory(2, "coding", "second")];
        let prompt = ConsolidationEngine::build_prompt("coding", &group);
        assert!(prompt.contains("first"));
        assert!(prompt.contains("second"));
        assert!(prompt.contains("These are 2 memories"));
        assert!(!prompt.contains("first\n1. "));
    }

    #[test]
    fn sanitize_drops_control_and_replacement_characters() {
        let cleaned = ConsolidationEngine::sanitize("a\u{0}b\u{FFFD}c\u{1b}d\n\n  e   f");
        assert_eq!(cleaned, "abcd e f");
    }

    #[test]
    fn sanitize_bounds_one_summary_to_the_budget() {
        let cleaned = ConsolidationEngine::sanitize(&"字".repeat(MAX_SUMMARY_BYTES));
        assert!(cleaned.len() <= MAX_SUMMARY_BYTES);
        // Never split a multi-byte character
        assert!(std::str::from_utf8(cleaned.as_bytes()).is_ok());
    }

    #[test]
    fn prompt_stops_at_the_entry_budget_instead_of_half_an_entry() {
        let mut group = Vec::new();
        for i in 0..MAX_GROUP_SIZE as i64 {
            let mut m = memory(i, "coding", &format!("title {i}"));
            m.summary = "x".repeat(MAX_SUMMARY_BYTES);
            group.push(m);
        }
        let prompt = ConsolidationEngine::build_prompt("coding", &group);
        assert!(prompt.len() < MAX_ENTRY_BYTES + 2_000, "prompt was {} bytes", prompt.len());
        // Whole entries only: every numbered line is followed by its body line.
        let numbered = prompt.matches("\n   x").count();
        assert!(numbered >= 1);
        assert!(prompt.matches("\n   x").count() == prompt.matches(" | entities: ").count());
    }
}
