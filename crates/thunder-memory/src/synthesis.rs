use crate::config::MemoryConfig;
use crate::db::{Database, NewLongTermMemory, RawCaptureRow};
use crate::error::MemoryError;
use crate::safe_truncate_str;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizedInsight {
    pub category: String,
    pub title: String,
    pub summary: String,
    pub key_entities: Vec<String>,
    pub importance_score: f32,
}

#[derive(Debug)]
pub struct CaptureEpisode {
    pub app_name: String,
    pub window_title: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub capture_ids: Vec<i64>,
    pub combined_text: String,
}

pub struct SynthesisEngine {
    db: Database,
    config: MemoryConfig,
    http_client: reqwest::Client,
}

impl SynthesisEngine {
    pub fn new(db: Database, config: MemoryConfig) -> Self {
        // The endpoint is what infers here — thunder-memory has no model of its own
        // any more. Nothing to load, so a wrong endpoint surfaces at the first
        // cycle (logged with its reason) rather than at construction time.
        if config.llm_endpoint.is_none() {
            debug!("No `llm_endpoint` configured, synthesis uses the deterministic rule extractor");
        }

        Self {
            db,
            config,
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Run one synthesis cycle: cluster unprocessed screen captures, extract insights, and commit to long-term memory
    pub async fn run_synthesis_cycle(&self) -> Result<usize, MemoryError> {
        let captures = self.db.get_unprocessed_captures(self.config.synthesis_batch_size)?;
        if captures.is_empty() {
            debug!("No unprocessed screen captures for synthesis");
            return Ok(0);
        }

        info!("Synthesizing insights from {} screen captures...", captures.len());

        let episodes = Self::cluster_into_episodes(captures);
        let mut synthesized_count = 0;

        for episode in episodes {
            let insight = self.extract_insight(&episode).await?;

            // Mem0-style Decision: ADD vs UPDATE
            let latest_active = self.db.find_latest_active_memory(&insight.category)?;
            let should_update = if let Some(ref active) = latest_active {
                // If within 15 minutes and same or overlapping topic/app
                let gap_ms = episode.start_ms - active.time_window_end_ms;
                gap_ms >= 0 && gap_ms <= 15 * 60 * 1000 && active.status == "active"
            } else {
                false
            };

            if should_update {
                if let Some(active) = latest_active {
                    info!("Updating ongoing long-term memory #{} with new episode", active.id);
                    self.db.update_long_term_memory(
                        active.id,
                        episode.end_ms,
                        episode.end_ms,
                        &insight.summary,
                        &insight.key_entities,
                    )?;
                }
            } else {
                info!("Adding new long-term memory: '{}' (score: {:.1})", insight.title, insight.importance_score);
                let new_mem = NewLongTermMemory {
                    category: insight.category,
                    title: insight.title,
                    summary: insight.summary,
                    key_entities: insight.key_entities,
                    importance_score: insight.importance_score,
                    time_window_start_ms: episode.start_ms,
                    time_window_end_ms: episode.end_ms,
                    source_capture_ids: episode.capture_ids.clone(),
                };
                self.db.insert_long_term_memory(&new_mem)?;
            }

            // Mark capture ids as processed
            self.db.mark_captures_processed(&episode.capture_ids)?;
            synthesized_count += 1;
        }

        Ok(synthesized_count)
    }

    /// Cluster consecutive captures into cohesive work episodes (by app and time continuity)
    pub fn cluster_into_episodes(captures: Vec<RawCaptureRow>) -> Vec<CaptureEpisode> {
        let mut episodes: Vec<CaptureEpisode> = Vec::new();

        for cap in captures {
            let can_merge = if let Some(last) = episodes.last_mut() {
                let time_gap = cap.captured_at_ms - last.end_ms;
                // Merge if same app and within 3 minutes (180,000 ms)
                last.app_name == cap.app_name && time_gap <= 180_000
            } else {
                false
            };

            if can_merge {
                let last = episodes.last_mut().unwrap();
                last.end_ms = cap.captured_at_ms;
                last.capture_ids.push(cap.id);
                if !cap.window_title.is_empty() {
                    last.window_title = cap.window_title.clone();
                }
                // Append unique non-empty text lines to combined_text
                for line in cap.ocr_text.lines() {
                    let trimmed = line.trim();
                    if trimmed.len() > 3 && !last.combined_text.contains(trimmed) {
                        last.combined_text.push_str(trimmed);
                        last.combined_text.push('\n');
                    }
                }
            } else {
                let mut combined_text = String::new();
                for line in cap.ocr_text.lines() {
                    let trimmed = line.trim();
                    if trimmed.len() > 3 {
                        combined_text.push_str(trimmed);
                        combined_text.push('\n');
                    }
                }

                episodes.push(CaptureEpisode {
                    app_name: cap.app_name,
                    window_title: cap.window_title,
                    start_ms: cap.captured_at_ms,
                    end_ms: cap.captured_at_ms,
                    capture_ids: vec![cap.id],
                    combined_text,
                });
            }
        }

        episodes
    }

    /// Extract structured insight from an episode using the configured LLM endpoint,
    /// falling back to the deterministic rule extractor.
    pub async fn extract_insight(&self, episode: &CaptureEpisode) -> Result<SynthesizedInsight, MemoryError> {
        // 1. The configured small-model endpoint (Ollama, MLX, a gateway — anything
        //    OpenAI-compatible). This is the only model-backed path there is: the
        //    in-process Candle engine was retired so that the model lives outside
        //    this process, where it can be swapped and shared.
        if let Some(ref endpoint) = self.config.llm_endpoint {
            match self.call_llm_endpoint(endpoint, episode).await {
                Ok(insight) => return Ok(insight),
                Err(e) => {
                    warn!("LLM endpoint {} failed: {}, falling back to heuristic extraction", endpoint, e);
                }
            }
        }

        // 2. Fast, deterministic heuristic extractor
        Ok(Self::heuristic_extract(episode))
    }

    /// Call an OpenAI-compatible small model endpoint (e.g. Ollama, Apple MLX, local server)
    async fn call_llm_endpoint(
        &self,
        endpoint: &str,
        episode: &CaptureEpisode,
    ) -> Result<SynthesizedInsight, MemoryError> {
        let url = if endpoint.ends_with("/chat/completions") {
            endpoint.to_string()
        } else {
            format!("{}/chat/completions", endpoint.trim_end_matches('/'))
        };

        let prompt = format!(
            r#"You are an AI Memory Consolidation Engine.
Analyze the following desktop OCR activity stream and extract key insights:
- App Name: {}
- Window Title: {}
- Extracted Screen Text:
{}

Respond ONLY with a valid JSON object matching this schema:
{{
  "category": "code_development" | "debugging" | "research" | "documentation" | "decision" | "communication",
  "title": "<Concise action/task title, under 10 words>",
  "summary": "<2-3 sentences explaining what was being done, problems solved, or context>",
  "key_entities": ["<entity1>", "<entity2>"],
  "importance_score": <1.0 to 10.0 float>
}}"#,
            episode.app_name,
            episode.window_title,
            if episode.combined_text.len() > 2500 {
                safe_truncate_str(&episode.combined_text, 2500)
            } else {
                &episode.combined_text
            }
        );

        let mut req = self.http_client.post(&url).json(&serde_json::json!({
            "model": &self.config.llm_model,
            "messages": [
                {"role": "system", "content": "You are a desktop activity memory synthesizer. Output only valid JSON."},
                {"role": "user", "content": prompt}
            ],
            "temperature": 0.2,
            "response_format": {"type": "json_object"}
        }));

        if let Some(ref key) = self.config.llm_api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(MemoryError::Synthesis(format!(
                "HTTP {} from LLM endpoint: {}",
                status, body
            )));
        }

        let json_body: serde_json::Value = resp.json().await?;
        let content = json_body["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| MemoryError::Synthesis("Invalid LLM response format".to_string()))?;

        let insight: SynthesizedInsight = serde_json::from_str(content).map_err(|e| {
            MemoryError::Synthesis(format!("Failed to parse JSON insight from LLM: {} (raw: {})", e, content))
        })?;

        Ok(insight)
    }

    /// Robust rule-based / heuristic extractor when LLM is unavailable
    pub fn heuristic_extract(episode: &CaptureEpisode) -> SynthesizedInsight {
        let app_lower = episode.app_name.to_lowercase();
        let title_lower = episode.window_title.to_lowercase();
        let text_lower = episode.combined_text.to_lowercase();

        // 1. Determine category
        let category = if app_lower.contains("code")
            || app_lower.contains("cursor")
            || app_lower.contains("xcode")
            || app_lower.contains("rustrover")
            || app_lower.contains("intellij")
            || title_lower.ends_with(".rs")
            || title_lower.ends_with(".ts")
            || title_lower.ends_with(".py")
            || title_lower.ends_with(".java")
        {
            if text_lower.contains("error:") || text_lower.contains("failed") || text_lower.contains("panic") {
                "debugging".to_string()
            } else {
                "code_development".to_string()
            }
        } else if app_lower.contains("terminal") || app_lower.contains("warp") || app_lower.contains("iterm") {
            if text_lower.contains("git commit") || text_lower.contains("cargo build") || text_lower.contains("docker") {
                "code_development".to_string()
            } else {
                "task_progress".to_string()
            }
        } else if app_lower.contains("chrome")
            || app_lower.contains("safari")
            || app_lower.contains("firefox")
            || app_lower.contains("edge")
        {
            if text_lower.contains("github.com") || text_lower.contains("stackoverflow") || text_lower.contains("docs.") {
                "documentation".to_string()
            } else {
                "research".to_string()
            }
        } else if app_lower.contains("slack")
            || app_lower.contains("wechat")
            || app_lower.contains("discord")
            || app_lower.contains("telegram")
        {
            "communication".to_string()
        } else {
            "task_progress".to_string()
        };

        // 2. Extract key entities (files, tools, commands)
        let mut entities = Vec::new();
        entities.push(episode.app_name.clone());

        for word in episode.combined_text.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-');
            if (cleaned.contains('.') || cleaned.contains('/'))
                && (cleaned.ends_with(".rs")
                    || cleaned.ends_with(".ts")
                    || cleaned.ends_with(".js")
                    || cleaned.ends_with(".json")
                    || cleaned.ends_with(".toml")
                    || cleaned.ends_with(".swift")
                    || cleaned.ends_with(".md"))
                && !entities.contains(&cleaned.to_string())
            {
                entities.push(cleaned.to_string());
            }
        }

        // 3. Compute importance score (1.0 .. 10.0)
        let mut importance = 4.0;
        if category == "debugging" || text_lower.contains("error") {
            importance += 2.5;
        }
        if category == "code_development" {
            importance += 1.5;
        }
        if text_lower.contains("git commit") || text_lower.contains("pr") || text_lower.contains("merge") {
            importance += 1.5;
        }
        if importance > 10.0 {
            importance = 10.0;
        }

        // 4. Synthesize title & summary
        let title = if !episode.window_title.is_empty() {
            format!("Working in {} - {}", episode.app_name, episode.window_title)
        } else {
            format!("Active in {}", episode.app_name)
        };

        let summary = format!(
            "Observed activity in {} over {} seconds. Key context: {}",
            episode.app_name,
            ((episode.end_ms - episode.start_ms).max(1000)) / 1000,
            if episode.combined_text.len() > 200 {
                format!(
                    "{}...",
                    safe_truncate_str(&episode.combined_text, 200).replace('\n', " ")
                )
            } else {
                episode.combined_text.replace('\n', " ")
            }
        );

        SynthesizedInsight {
            category,
            title,
            summary,
            key_entities: entities,
            importance_score: importance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_episode_clustering() {
        let captures = vec![
            RawCaptureRow {
                id: 1,
                captured_at_ms: 1000,
                app_name: "Cursor".into(),
                bundle_id: "com.cursor".into(),
                window_title: "main.rs".into(),
                ocr_text: "fn main() {}".into(),
                confidence: 0.9,
                content_hash: "h1".into(),
                metadata: "".into(),
                processed: false,
            },
            RawCaptureRow {
                id: 2,
                captured_at_ms: 5000,
                app_name: "Cursor".into(),
                bundle_id: "com.cursor".into(),
                window_title: "main.rs".into(),
                ocr_text: "fn main() { println!(); }".into(),
                confidence: 0.95,
                content_hash: "h2".into(),
                metadata: "".into(),
                processed: false,
            },
            RawCaptureRow {
                id: 3,
                captured_at_ms: 300_000, // 5 minutes later
                app_name: "Chrome".into(),
                bundle_id: "com.chrome".into(),
                window_title: "Rust Docs".into(),
                ocr_text: "std::sync::Arc".into(),
                confidence: 0.9,
                content_hash: "h3".into(),
                metadata: "".into(),
                processed: false,
            },
        ];

        let episodes = SynthesisEngine::cluster_into_episodes(captures);
        assert_eq!(episodes.len(), 2, "Should cluster into 2 episodes (Cursor vs Chrome)");
        assert_eq!(episodes[0].capture_ids, vec![1, 2]);
        assert_eq!(episodes[1].capture_ids, vec![3]);
    }

    #[test]
    fn test_heuristic_extract() {
        let episode = CaptureEpisode {
            app_name: "Cursor".into(),
            window_title: "db.rs".into(),
            start_ms: 1000,
            end_ms: 20000,
            capture_ids: vec![1, 2],
            combined_text: "error: mismatched types in crates/thunder-memory/src/db.rs\nexpected struct rusqlite::Error".into(),
        };

        let insight = SynthesisEngine::heuristic_extract(&episode);
        assert_eq!(insight.category, "debugging");
        assert!(insight.importance_score >= 6.0);
        assert!(insight.key_entities.contains(&"Cursor".to_string()));
        assert!(insight.key_entities.iter().any(|e| e.contains("db.rs")));
    }
}
