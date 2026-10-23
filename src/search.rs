use crate::db::{Database, LongTermMemoryRow};
use crate::error::MemoryError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredMemory {
    pub memory: LongTermMemoryRow,
    pub total_score: f32,
    pub recency_score: f32,
    pub importance_score_norm: f32,
    pub relevance_score: f32,
    pub utility_score_norm: f32,
}

pub struct MemorySearchEngine {
    db: Database,
}

impl MemorySearchEngine {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Retrieve memories ranked by Stanford Generative Agents retrieval score:
    /// Score = w_recency * Recency + w_importance * Importance + w_relevance * Relevance
    pub fn query(
        &self,
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<ScoredMemory>, MemoryError> {
        // Superseded memories were folded into a consolidated one; scoring them
        // again would cite the same work twice.
        let candidates = self.db.list_recent_active_memories(limit * 3)?;
        let query_terms: Vec<String> = query_text
            .to_lowercase()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let mut scored: Vec<ScoredMemory> = candidates
            .into_iter()
            .map(|m| {
                // 1. Recency: exponential decay with half-life of 2 days (172,800,000 ms)
                let age_ms = (now_ms - m.updated_at_ms).max(0) as f64;
                let half_life_ms = 172_800_000.0;
                let recency_score = (-0.693 * (age_ms / half_life_ms)).exp() as f32;

                // 2. Importance score: normalized from 1.0..10.0 to 0.1..1.0
                let importance_score_norm = (m.importance_score / 10.0).clamp(0.1, 1.0);

                // 3. Relevance score: lexical overlap ratio across title, summary, and key_entities
                let mut matched_terms = 0;
                let text_corpus = format!(
                    "{} {} {}",
                    m.title.to_lowercase(),
                    m.summary.to_lowercase(),
                    m.key_entities.join(" ").to_lowercase()
                );

                for term in &query_terms {
                    if text_corpus.contains(term) {
                        matched_terms += 1;
                    }
                }

                let relevance_score = if query_terms.is_empty() {
                    0.5
                } else {
                    (matched_terms as f32) / (query_terms.len() as f32)
                };

                // 4. Utility feedback score: normalized from baseline 1.0
                let utility_score_norm = (m.utility_score / 2.0).clamp(0.1, 1.0);

                // Composite score (weights: 0.20 recency, 0.20 importance, 0.40 relevance, 0.20 utility)
                let total_score = (0.20 * recency_score)
                    + (0.20 * importance_score_norm)
                    + (0.40 * relevance_score)
                    + (0.20 * utility_score_norm);

                ScoredMemory {
                    memory: m,
                    total_score,
                    recency_score,
                    importance_score_norm,
                    relevance_score,
                    utility_score_norm,
                }
            })
            .collect();

        // Sort descending by total_score
        scored.sort_by(|a, b| {
            b.total_score
                .partial_cmp(&a.total_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        if scored.len() > limit {
            scored.truncate(limit);
        }

        Ok(scored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::NewLongTermMemory;

    #[test]
    fn test_memory_scoring() {
        let db = Database::in_memory().unwrap();

        let mem1 = NewLongTermMemory {
            category: "code_development".into(),
            title: "Developing thunder-memory module in Rust".into(),
            summary: "Building SQLite and OCR perception layer".into(),
            key_entities: vec!["Rust".into(), "SQLite".into(), "OCR".into()],
            importance_score: 9.0,
            time_window_start_ms: 1000,
            time_window_end_ms: 2000,
            source_capture_ids: vec![1],
        };
        db.insert_long_term_memory(&mem1).unwrap();

        let mem2 = NewLongTermMemory {
            category: "communication".into(),
            title: "WeChat chatting".into(),
            summary: "Random discussion about lunch".into(),
            key_entities: vec!["WeChat".into()],
            importance_score: 2.0,
            time_window_start_ms: 1000,
            time_window_end_ms: 2000,
            source_capture_ids: vec![2],
        };
        db.insert_long_term_memory(&mem2).unwrap();

        let search = MemorySearchEngine::new(db);
        let results = search.query("Rust SQLite", 5).unwrap();

        assert!(!results.is_empty());
        assert_eq!(results[0].memory.title, "Developing thunder-memory module in Rust");
        assert!(results[0].total_score > results[1].total_score);
    }

    #[test]
    fn test_utility_score_boost() {
        let db = Database::in_memory().unwrap();

        let mem1 = NewLongTermMemory {
            category: "code_development".into(),
            title: "Architecture Design A".into(),
            summary: "Design pattern A details".into(),
            key_entities: vec!["pattern".into()],
            importance_score: 5.0,
            time_window_start_ms: 1000,
            time_window_end_ms: 2000,
            source_capture_ids: vec![1],
        };
        let id1 = db.insert_long_term_memory(&mem1).unwrap();

        let mem2 = NewLongTermMemory {
            category: "code_development".into(),
            title: "Architecture Design B".into(),
            summary: "Design pattern B details".into(),
            key_entities: vec!["pattern".into()],
            importance_score: 5.0,
            time_window_start_ms: 1000,
            time_window_end_ms: 2000,
            source_capture_ids: vec![2],
        };
        let id2 = db.insert_long_term_memory(&mem2).unwrap();

        // Reinforce mem2 with positive utility feedback
        db.track_memory_access("long_term", id2, 1.5).unwrap();

        let search = MemorySearchEngine::new(db);
        let results = search.query("pattern", 5).unwrap();

        assert_eq!(results.len(), 2);
        // mem2 should rank first because of higher utility score
        assert_eq!(results[0].memory.id, id2);
        assert_eq!(results[1].memory.id, id1);
        assert!(results[0].total_score > results[1].total_score);
        assert!(results[0].utility_score_norm > results[1].utility_score_norm);
    }
}
