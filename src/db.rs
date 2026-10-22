use crate::capture::ScreenCaptureRecord;
use crate::error::MemoryError;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tracing::debug;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawCaptureRow {
    pub id: i64,
    pub captured_at_ms: i64,
    pub app_name: String,
    pub bundle_id: String,
    pub window_title: String,
    pub ocr_text: String,
    pub confidence: f32,
    pub content_hash: String,
    pub metadata: String,
    pub processed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLongTermMemory {
    pub category: String,
    pub title: String,
    pub summary: String,
    pub key_entities: Vec<String>,
    pub importance_score: f32,
    pub time_window_start_ms: i64,
    pub time_window_end_ms: i64,
    pub source_capture_ids: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongTermMemoryRow {
    pub id: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub category: String,
    pub title: String,
    pub summary: String,
    pub key_entities: Vec<String>,
    pub importance_score: f32,
    pub time_window_start_ms: i64,
    pub time_window_end_ms: i64,
    pub source_capture_ids: Vec<i64>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposerTrainingSample {
    pub id: String,
    pub created_at_ms: i64,
    pub project_path: String,
    pub pane_key: String,
    pub provider: Option<String>,
    pub agent_session_id: Option<String>,
    pub raw_text: String,
    pub intent_category: String,
    pub extracted_entities: Vec<String>,
    pub quality_score: f32,
    pub training_prompt: String,
    pub synced_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_captures: i64,
    pub unprocessed_captures: i64,
    pub long_term_memories: i64,
    pub training_samples: i64,
    pub last_capture_ms: Option<i64>,
    pub last_synthesis_ms: Option<i64>,
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MemoryError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            "#,
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn init_schema(&self) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        conn.execute_batch(
            r#"
            -- Tier 1: Screen OCR perception stream
            CREATE TABLE IF NOT EXISTS screen_captures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                captured_at_ms INTEGER NOT NULL,
                app_name TEXT NOT NULL,
                bundle_id TEXT NOT NULL,
                window_title TEXT NOT NULL,
                ocr_text TEXT NOT NULL,
                confidence REAL NOT NULL,
                content_hash TEXT NOT NULL,
                metadata TEXT,
                processed INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_screen_captures_captured_at
                ON screen_captures(captured_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_screen_captures_processed
                ON screen_captures(processed, captured_at_ms ASC);
            CREATE INDEX IF NOT EXISTS idx_screen_captures_hash
                ON screen_captures(content_hash);
            CREATE INDEX IF NOT EXISTS idx_screen_captures_app
                ON screen_captures(app_name, captured_at_ms DESC);

            -- Tier 2: Synthesized long-term semantic & episodic memory
            CREATE TABLE IF NOT EXISTS long_term_memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                key_entities TEXT NOT NULL,
                importance_score REAL NOT NULL,
                time_window_start_ms INTEGER NOT NULL,
                time_window_end_ms INTEGER NOT NULL,
                source_capture_ids TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
            );

            CREATE INDEX IF NOT EXISTS idx_long_term_memories_created
                ON long_term_memories(created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_category
                ON long_term_memories(category, created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_importance
                ON long_term_memories(importance_score DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_status
                ON long_term_memories(status, updated_at_ms DESC);

            -- Tier 3: Ingested workbench_composer_sends training data
            CREATE TABLE IF NOT EXISTS composer_training_samples (
                id TEXT PRIMARY KEY,
                created_at_ms INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                pane_key TEXT NOT NULL,
                provider TEXT,
                agent_session_id TEXT,
                raw_text TEXT NOT NULL,
                intent_category TEXT NOT NULL,
                extracted_entities TEXT NOT NULL,
                quality_score REAL NOT NULL,
                training_prompt TEXT NOT NULL,
                synced_at_ms INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_composer_training_created
                ON composer_training_samples(created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_composer_training_quality
                ON composer_training_samples(quality_score DESC);
            CREATE INDEX IF NOT EXISTS idx_composer_training_intent
                ON composer_training_samples(intent_category);
            CREATE INDEX IF NOT EXISTS idx_composer_training_project
                ON composer_training_samples(project_path, created_at_ms DESC);

            -- Metadata key-value store for watermarks and sync states
            CREATE TABLE IF NOT EXISTS memory_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    /// Insert a raw screen capture record into screen_captures
    pub fn insert_capture(&self, record: &ScreenCaptureRecord) -> Result<i64, MemoryError> {
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO screen_captures (
                captured_at_ms, app_name, bundle_id, window_title,
                ocr_text, confidence, content_hash, metadata, processed
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)
            "#,
            params![
                record.timestamp_ms,
                record.app_name,
                record.bundle_id,
                record.window_title,
                record.full_text,
                record.avg_confidence,
                record.content_hash,
                record.metadata_json,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Get the hash of the most recent screen capture record
    pub fn get_last_capture_hash(&self) -> Result<Option<String>, MemoryError> {
        let conn = self.conn.lock();
        let hash: Option<String> = conn
            .query_row(
                "SELECT content_hash FROM screen_captures ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(hash)
    }

    /// Retrieve unprocessed captures for synthesis
    pub fn get_unprocessed_captures(&self, limit: usize) -> Result<Vec<RawCaptureRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, captured_at_ms, app_name, bundle_id, window_title,
                   ocr_text, confidence, content_hash, metadata, processed
            FROM screen_captures
            WHERE processed = 0
            ORDER BY captured_at_ms ASC
            LIMIT ?1
            "#,
        )?;

        let rows = stmt.query_map(params![limit as i64], |row| {
            let processed_int: i64 = row.get(9)?;
            Ok(RawCaptureRow {
                id: row.get(0)?,
                captured_at_ms: row.get(1)?,
                app_name: row.get(2)?,
                bundle_id: row.get(3)?,
                window_title: row.get(4)?,
                ocr_text: row.get(5)?,
                confidence: row.get(6)?,
                content_hash: row.get(7)?,
                metadata: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                processed: processed_int != 0,
            })
        })?;

        let mut result = Vec::new();
        for r in rows {
            result.push(r?);
        }
        Ok(result)
    }

    /// Mark capture records as processed by synthesis engine
    pub fn mark_captures_processed(&self, ids: &[i64]) -> Result<(), MemoryError> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock();
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE screen_captures SET processed = 1 WHERE id IN ({})",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let rusqlite_params: Vec<&dyn rusqlite::ToSql> =
            ids.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
        stmt.execute(rusqlite_params.as_slice())?;
        debug!("Marked {} screen_captures as processed", ids.len());
        Ok(())
    }

    /// Insert new consolidated long-term memory
    pub fn insert_long_term_memory(&self, memory: &NewLongTermMemory) -> Result<i64, MemoryError> {
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let entities_json = serde_json::to_string(&memory.key_entities).unwrap_or_else(|_| "[]".to_string());
        let sources_json = serde_json::to_string(&memory.source_capture_ids).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO long_term_memories (
                created_at_ms, updated_at_ms, category, title, summary,
                key_entities, importance_score, time_window_start_ms,
                time_window_end_ms, source_capture_ids, status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active')
            "#,
            params![
                now_ms,
                now_ms,
                memory.category,
                memory.title,
                memory.summary,
                entities_json,
                memory.importance_score,
                memory.time_window_start_ms,
                memory.time_window_end_ms,
                sources_json,
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Update an ongoing long-term memory (Mem0-style UPDATE)
    pub fn update_long_term_memory(
        &self,
        id: i64,
        updated_at_ms: i64,
        window_end_ms: i64,
        additional_summary: &str,
        new_entities: &[String],
    ) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        // Read existing memory to append summary and merge entities
        let existing: Option<(String, String)> = conn
            .query_row(
                "SELECT summary, key_entities FROM long_term_memories WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        if let Some((curr_summary, curr_entities_json)) = existing {
            let mut entities: Vec<String> =
                serde_json::from_str(&curr_entities_json).unwrap_or_default();
            for ent in new_entities {
                if !entities.contains(ent) {
                    entities.push(ent.clone());
                }
            }
            let merged_entities = serde_json::to_string(&entities).unwrap_or_else(|_| "[]".to_string());

            let updated_summary = if additional_summary.is_empty() {
                curr_summary
            } else {
                format!("{}\n\n[Progress Update]: {}", curr_summary, additional_summary)
            };

            conn.execute(
                r#"
                UPDATE long_term_memories
                SET updated_at_ms = ?1,
                    time_window_end_ms = ?2,
                    summary = ?3,
                    key_entities = ?4
                WHERE id = ?5
                "#,
                params![
                    updated_at_ms,
                    window_end_ms,
                    updated_summary,
                    merged_entities,
                    id,
                ],
            )?;
        }
        Ok(())
    }

    /// Find latest active long-term memory for an app/category to determine if we should ADD or UPDATE
    pub fn find_latest_active_memory(
        &self,
        category: &str,
    ) -> Result<Option<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let row: Option<LongTermMemoryRow> = conn
            .query_row(
                r#"
                SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                       key_entities, importance_score, time_window_start_ms,
                       time_window_end_ms, source_capture_ids, status
                FROM long_term_memories
                WHERE category = ?1 AND status = 'active'
                ORDER BY updated_at_ms DESC
                LIMIT 1
                "#,
                params![category],
                Self::map_long_term_row,
            )
            .optional()?;
        Ok(row)
    }

    /// Search long-term memories using query keywords and recency
    pub fn search_long_term_memories(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                   key_entities, importance_score, time_window_start_ms,
                   time_window_end_ms, source_capture_ids, status
            FROM long_term_memories
            WHERE title LIKE ?1 OR summary LIKE ?1 OR key_entities LIKE ?1
            ORDER BY importance_score DESC, updated_at_ms DESC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![pattern, limit as i64], Self::map_long_term_row)?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    /// List recent long-term memories
    pub fn list_recent_memories(&self, limit: usize) -> Result<Vec<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                   key_entities, importance_score, time_window_start_ms,
                   time_window_end_ms, source_capture_ids, status
            FROM long_term_memories
            ORDER BY updated_at_ms DESC
            LIMIT ?1
            "#,
        )?;

        let rows = stmt.query_map(params![limit as i64], Self::map_long_term_row)?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    fn map_long_term_row(row: &rusqlite::Row) -> rusqlite::Result<LongTermMemoryRow> {
        let entities_str: String = row.get(6)?;
        let entities: Vec<String> = serde_json::from_str(&entities_str).unwrap_or_default();
        let sources_str: String = row.get(10)?;
        let sources: Vec<i64> = serde_json::from_str(&sources_str).unwrap_or_default();

        Ok(LongTermMemoryRow {
            id: row.get(0)?,
            created_at_ms: row.get(1)?,
            updated_at_ms: row.get(2)?,
            category: row.get(3)?,
            title: row.get(4)?,
            summary: row.get(5)?,
            key_entities: entities,
            importance_score: row.get(7)?,
            time_window_start_ms: row.get(8)?,
            time_window_end_ms: row.get(9)?,
            source_capture_ids: sources,
            status: row.get(11)?,
        })
    }

    /// Ingest a composer sample into composer_training_samples
    pub fn insert_composer_sample(
        &self,
        sample: &ComposerTrainingSample,
    ) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        let entities_json = serde_json::to_string(&sample.extracted_entities).unwrap_or_else(|_| "[]".to_string());

        conn.execute(
            r#"
            INSERT INTO composer_training_samples (
                id, created_at_ms, project_path, pane_key, provider,
                agent_session_id, raw_text, intent_category,
                extracted_entities, quality_score, training_prompt, synced_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                raw_text = excluded.raw_text,
                quality_score = excluded.quality_score,
                training_prompt = excluded.training_prompt,
                synced_at_ms = excluded.synced_at_ms
            "#,
            params![
                sample.id,
                sample.created_at_ms,
                sample.project_path,
                sample.pane_key,
                sample.provider,
                sample.agent_session_id,
                sample.raw_text,
                sample.intent_category,
                entities_json,
                sample.quality_score,
                sample.training_prompt,
                sample.synced_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Get last composer sync timestamp
    pub fn get_meta_i64(&self, key: &str) -> Result<Option<i64>, MemoryError> {
        let conn = self.conn.lock();
        let val: Option<String> = conn
            .query_row(
                "SELECT value FROM memory_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(val.and_then(|v| v.parse::<i64>().ok()))
    }

    /// Set metadata key-value
    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        conn.execute(
            r#"
            INSERT INTO memory_meta (key, value, updated_at_ms)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
            params![key, value, now_ms],
        )?;
        Ok(())
    }

    /// Retrieve training samples filtered by minimum quality score
    pub fn get_training_samples(
        &self,
        min_quality: f32,
        limit: Option<usize>,
    ) -> Result<Vec<ComposerTrainingSample>, MemoryError> {
        let conn = self.conn.lock();
        let limit_sql = limit.map(|l| format!("LIMIT {}", l)).unwrap_or_default();
        let query = format!(
            r#"
            SELECT id, created_at_ms, project_path, pane_key, provider,
                   agent_session_id, raw_text, intent_category,
                   extracted_entities, quality_score, training_prompt, synced_at_ms
            FROM composer_training_samples
            WHERE quality_score >= ?1
            ORDER BY created_at_ms ASC
            {}
            "#,
            limit_sql
        );

        let mut stmt = conn.prepare(&query)?;
        let rows = stmt.query_map(params![min_quality], |row| {
            let entities_str: String = row.get(8)?;
            let entities: Vec<String> = serde_json::from_str(&entities_str).unwrap_or_default();
            Ok(ComposerTrainingSample {
                id: row.get(0)?,
                created_at_ms: row.get(1)?,
                project_path: row.get(2)?,
                pane_key: row.get(3)?,
                provider: row.get(4)?,
                agent_session_id: row.get(5)?,
                raw_text: row.get(6)?,
                intent_category: row.get(7)?,
                extracted_entities: entities,
                quality_score: row.get(9)?,
                training_prompt: row.get(10)?,
                synced_at_ms: row.get(11)?,
            })
        })?;

        let mut samples = Vec::new();
        for r in rows {
            samples.push(r?);
        }
        Ok(samples)
    }

    /// Retrieve overall statistics
    pub fn get_stats(&self) -> Result<MemoryStats, MemoryError> {
        let conn = self.conn.lock();

        let total_captures: i64 = conn
            .query_row("SELECT COUNT(*) FROM screen_captures", [], |r| r.get(0))
            .unwrap_or(0);
        let unprocessed_captures: i64 = conn
            .query_row("SELECT COUNT(*) FROM screen_captures WHERE processed = 0", [], |r| r.get(0))
            .unwrap_or(0);
        let long_term_memories: i64 = conn
            .query_row("SELECT COUNT(*) FROM long_term_memories", [], |r| r.get(0))
            .unwrap_or(0);
        let training_samples: i64 = conn
            .query_row("SELECT COUNT(*) FROM composer_training_samples", [], |r| r.get(0))
            .unwrap_or(0);

        let last_capture_ms: Option<i64> = conn
            .query_row(
                "SELECT captured_at_ms FROM screen_captures ORDER BY captured_at_ms DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;

        let last_synthesis_ms: Option<i64> = conn
            .query_row(
                "SELECT updated_at_ms FROM long_term_memories ORDER BY updated_at_ms DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;

        Ok(MemoryStats {
            total_captures,
            unprocessed_captures,
            long_term_memories,
            training_samples,
            last_capture_ms,
            last_synthesis_ms,
        })
    }

    /// Prune raw screen captures older than retention days
    pub fn prune_old_captures(&self, retention_days: u32) -> Result<usize, MemoryError> {
        let cutoff_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
            - (retention_days as i64 * 86_400_000);

        let conn = self.conn.lock();
        let count = conn.execute(
            "DELETE FROM screen_captures WHERE captured_at_ms < ?1 AND processed = 1",
            params![cutoff_ms],
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_and_captures() {
        let db = Database::in_memory().unwrap();

        let rec = ScreenCaptureRecord {
            timestamp_ms: 1000,
            app_name: "Cursor".into(),
            bundle_id: "com.cursor".into(),
            window_title: "lib.rs".into(),
            full_text: "fn test() {}".into(),
            avg_confidence: 0.9,
            content_hash: "hash123".into(),
            metadata_json: "[]".into(),
        };

        let id = db.insert_capture(&rec).unwrap();
        assert_eq!(id, 1);

        let hash = db.get_last_capture_hash().unwrap();
        assert_eq!(hash, Some("hash123".into()));

        let unprocessed = db.get_unprocessed_captures(10).unwrap();
        assert_eq!(unprocessed.len(), 1);
        assert_eq!(unprocessed[0].app_name, "Cursor");

        db.mark_captures_processed(&[id]).unwrap();
        let unprocessed2 = db.get_unprocessed_captures(10).unwrap();
        assert_eq!(unprocessed2.len(), 0);
    }

    #[test]
    fn test_long_term_memories_add_and_update() {
        let db = Database::in_memory().unwrap();

        let mem = NewLongTermMemory {
            category: "code_development".into(),
            title: "Implementing SQLite Layer".into(),
            summary: "Created database schema for thunder-memory".into(),
            key_entities: vec!["rusqlite".into(), "SQLite".into()],
            importance_score: 8.5,
            time_window_start_ms: 1000,
            time_window_end_ms: 2000,
            source_capture_ids: vec![1, 2],
        };

        let mem_id = db.insert_long_term_memory(&mem).unwrap();
        assert_eq!(mem_id, 1);

        db.update_long_term_memory(
            mem_id,
            2500,
            2500,
            "Added indexes and WAL mode",
            &["WAL".into()],
        )
        .unwrap();

        let results = db.search_long_term_memories("SQLite", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("Added indexes and WAL mode"));
        assert!(results[0].key_entities.contains(&"WAL".into()));
    }
}
