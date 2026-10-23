use crate::capture::ScreenCaptureRecord;
use crate::error::MemoryError;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
    pub utility_score: f32,
    pub access_count: i64,
    pub last_accessed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionMask {
    pub id: i64,
    pub bundle_id: String,
    pub window_title_pattern: String,
    pub reason: String,
    pub hit_count: i64,
    pub is_active: bool,
    pub created_at_ms: i64,
    pub last_hit_at_ms: i64,
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
pub struct NewAgentInteraction {
    pub agent_name: String,
    pub scope: String, // "global" | "project"
    pub project_path: Option<String>,
    pub category: String, // "constraint", "user_preference", "architecture_decision", "bug_solution", "task_milestone"
    pub title: String,
    pub content: String,
    pub entities: Vec<String>,
    pub suggested_importance: f32,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInteractionRow {
    pub id: i64,
    pub recorded_at_ms: i64,
    pub agent_name: String,
    pub scope: String,
    pub project_path: Option<String>,
    pub category: String,
    pub title: String,
    pub content: String,
    pub entities: Vec<String>,
    pub suggested_importance: f32,
    pub status: String,
    pub metadata: String,
    pub utility_score: f32,
    pub access_count: i64,
    pub last_accessed_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStats {
    pub total_captures: i64,
    pub unprocessed_captures: i64,
    pub long_term_memories: i64,
    pub agent_interactions: i64,
    pub training_samples: i64,
    pub suppression_mask_hits: i64,
    pub last_capture_ms: Option<i64>,
    pub last_synthesis_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionResult {
    pub pruned_captures: usize,
    pub archived_memories: usize,
    pub active_db_bytes_before: u64,
    pub active_db_bytes_after: u64,
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl Database {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, MemoryError> {
        let p = path.as_ref().to_path_buf();
        let conn = Connection::open(&p)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            "#,
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: Some(p),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn in_memory() -> Result<Self, MemoryError> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            path: None,
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
                status TEXT NOT NULL DEFAULT 'active',
                utility_score REAL NOT NULL DEFAULT 1.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed_at_ms INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_long_term_memories_created
                ON long_term_memories(created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_category
                ON long_term_memories(category, created_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_importance
                ON long_term_memories(importance_score DESC);
            CREATE INDEX IF NOT EXISTS idx_long_term_memories_status
                ON long_term_memories(status, updated_at_ms DESC);

            -- Tier 3: Agent explicit cognitive memory interactions
            CREATE TABLE IF NOT EXISTS agent_interactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                recorded_at_ms INTEGER NOT NULL,
                agent_name TEXT NOT NULL,
                scope TEXT NOT NULL DEFAULT 'global',
                project_path TEXT,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                entities TEXT NOT NULL DEFAULT '[]',
                suggested_importance REAL NOT NULL DEFAULT 5.0,
                status TEXT NOT NULL DEFAULT 'active',
                metadata TEXT NOT NULL DEFAULT '',
                utility_score REAL NOT NULL DEFAULT 1.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed_at_ms INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_agent_interactions_scope_cat
                ON agent_interactions(scope, category, status, recorded_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_agent_interactions_project
                ON agent_interactions(project_path, status, recorded_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_agent_interactions_status
                ON agent_interactions(status, recorded_at_ms DESC);

            -- Tier 4: Ingested workbench_composer_sends training data
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

            -- Autonomous Cognitive Noise Suppression Masks
            CREATE TABLE IF NOT EXISTS suppression_masks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                bundle_id TEXT NOT NULL,
                window_title_pattern TEXT NOT NULL,
                reason TEXT NOT NULL,
                hit_count INTEGER NOT NULL DEFAULT 0,
                is_active INTEGER NOT NULL DEFAULT 1,
                created_at_ms INTEGER NOT NULL,
                last_hit_at_ms INTEGER NOT NULL DEFAULT 0
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_suppression_masks_unique
                ON suppression_masks(bundle_id, window_title_pattern);
            CREATE INDEX IF NOT EXISTS idx_suppression_masks_active
                ON suppression_masks(is_active, hit_count DESC);

            -- Metadata key-value store for watermarks and sync states
            CREATE TABLE IF NOT EXISTS memory_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            "#,
        )?;

        // Safe additive column migrations for existing databases
        let _ = conn.execute("ALTER TABLE long_term_memories ADD COLUMN utility_score REAL NOT NULL DEFAULT 1.0;", []);
        let _ = conn.execute("ALTER TABLE long_term_memories ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;", []);
        let _ = conn.execute("ALTER TABLE long_term_memories ADD COLUMN last_accessed_at_ms INTEGER NOT NULL DEFAULT 0;", []);

        let _ = conn.execute("ALTER TABLE agent_interactions ADD COLUMN utility_score REAL NOT NULL DEFAULT 1.0;", []);
        let _ = conn.execute("ALTER TABLE agent_interactions ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;", []);
        let _ = conn.execute("ALTER TABLE agent_interactions ADD COLUMN last_accessed_at_ms INTEGER NOT NULL DEFAULT 0;", []);

        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_long_term_memories_utility ON long_term_memories(utility_score DESC, updated_at_ms DESC);",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_agent_interactions_utility ON agent_interactions(utility_score DESC, recorded_at_ms DESC);",
            [],
        );

        // Seed default noise suppression masks if table is empty
        let mask_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM suppression_masks", [], |r| r.get(0))
            .unwrap_or(0);

        if mask_count == 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let default_masks = [
                ("com.apple.Music", "*", "Apple Music player window has no cognitive development value"),
                ("com.spotify.client", "*", "Spotify music player window has no cognitive development value"),
                ("com.apple.QuickTimePlayerX", "*", "Video playback window has no text development value"),
                ("com.netease.163music", "*", "NetEase Music player window has no development value"),
            ];
            for (bundle, pattern, reason) in default_masks {
                let _ = conn.execute(
                    r#"
                    INSERT OR IGNORE INTO suppression_masks (
                        bundle_id, window_title_pattern, reason, hit_count, is_active, created_at_ms, last_hit_at_ms
                    ) VALUES (?1, ?2, ?3, 0, 1, ?4, 0)
                    "#,
                    params![bundle, pattern, reason, now],
                );
            }
        }
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
                       time_window_end_ms, source_capture_ids, status,
                       utility_score, access_count, last_accessed_at_ms
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
                   time_window_end_ms, source_capture_ids, status,
                   utility_score, access_count, last_accessed_at_ms
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

    /// List recent long-term memories, every status — the diagnostic view.
    ///
    /// Retrieval never comes through here: a memory folded into a later
    /// consolidation pass is still true historically, but re-serving it beside
    /// the consolidated one is how an agent ends up citing the same fact twice.
    pub fn list_recent_memories(&self, limit: usize) -> Result<Vec<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                   key_entities, importance_score, time_window_start_ms,
                   time_window_end_ms, source_capture_ids, status,
                   utility_score, access_count, last_accessed_at_ms
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

    /// List recent `active` long-term memories — the retrieval view.
    pub fn list_recent_active_memories(
        &self,
        limit: usize,
    ) -> Result<Vec<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                   key_entities, importance_score, time_window_start_ms,
                   time_window_end_ms, source_capture_ids, status,
                   utility_score, access_count, last_accessed_at_ms
            FROM long_term_memories
            WHERE status = 'active'
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

    /// Active long-term memories updated since `since_ms`, oldest first.
    ///
    /// Oldest first on purpose: the deep-consolidation pass reads them in the
    /// order they happened, which is the only order that makes "this episode
    /// continued that one" visible to the model.
    pub fn list_active_memories_since(
        &self,
        since_ms: i64,
        limit: usize,
    ) -> Result<Vec<LongTermMemoryRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, created_at_ms, updated_at_ms, category, title, summary,
                   key_entities, importance_score, time_window_start_ms,
                   time_window_end_ms, source_capture_ids, status,
                   utility_score, access_count, last_accessed_at_ms
            FROM long_term_memories
            WHERE status = 'active' AND updated_at_ms >= ?1
            ORDER BY time_window_start_ms ASC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![since_ms, limit as i64], Self::map_long_term_row)?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    /// Mark memories as folded into a consolidated one. Returns how many moved.
    pub fn supersede_memories(&self, ids: &[i64]) -> Result<usize, MemoryError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE long_term_memories SET status = 'superseded', updated_at_ms = ?1 \
             WHERE status = 'active' AND id IN ({placeholders})"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut query_params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        query_params.push(&now_ms);
        for id in ids {
            query_params.push(id);
        }
        let moved = stmt.execute(query_params.as_slice())?;
        debug!("Superseded {moved} long_term_memories");
        Ok(moved)
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
            utility_score: row.get(12).unwrap_or(1.0),
            access_count: row.get(13).unwrap_or(0),
            last_accessed_at_ms: row.get(14).unwrap_or(0),
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
        let agent_interactions: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_interactions", [], |r| r.get(0))
            .unwrap_or(0);
        let training_samples: i64 = conn
            .query_row("SELECT COUNT(*) FROM composer_training_samples", [], |r| r.get(0))
            .unwrap_or(0);
        let suppression_mask_hits: i64 = conn
            .query_row("SELECT COALESCE(SUM(hit_count), 0) FROM suppression_masks", [], |r| r.get(0))
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
            agent_interactions,
            training_samples,
            suppression_mask_hits,
            last_capture_ms,
            last_synthesis_ms,
        })
    }

    /// Insert a structured cognitive memory recorded by an agent.
    /// If the same title exists in the same scope and project, supersedes the previous active record.
    pub fn insert_agent_interaction(
        &self,
        item: &NewAgentInteraction,
    ) -> Result<i64, MemoryError> {
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let entities_json = serde_json::to_string(&item.entities).unwrap_or_else(|_| "[]".to_string());
        let meta = item.metadata.clone().unwrap_or_default();

        // Check if there is an active record with same scope, project_path, and title
        // If so, mark it as 'superseded' to maintain versioned evolution
        if let Some(ref proj) = item.project_path {
            conn.execute(
                r#"
                UPDATE agent_interactions
                SET status = 'superseded'
                WHERE scope = ?1 AND project_path = ?2 AND title = ?3 AND status = 'active'
                "#,
                params![item.scope, proj, item.title],
            )?;
        } else {
            conn.execute(
                r#"
                UPDATE agent_interactions
                SET status = 'superseded'
                WHERE scope = ?1 AND project_path IS NULL AND title = ?2 AND status = 'active'
                "#,
                params![item.scope, item.title],
            )?;
        }

        conn.execute(
            r#"
            INSERT INTO agent_interactions (
                recorded_at_ms, agent_name, scope, project_path,
                category, title, content, entities, suggested_importance,
                status, metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'active', ?10)
            "#,
            params![
                now_ms,
                item.agent_name,
                item.scope,
                item.project_path,
                item.category,
                item.title,
                item.content,
                entities_json,
                item.suggested_importance,
                meta,
            ],
        )?;

        let id = conn.last_insert_rowid();
        Ok(id)
    }

    /// Retrieve active constraints, user preferences, and architectural rules.
    /// Filters by scope: all 'global' records PLUS 'project' records matching project_path.
    pub fn get_active_preferences(
        &self,
        target_project_path: Option<&str>,
    ) -> Result<Vec<AgentInteractionRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut results = Vec::new();

        if let Some(proj) = target_project_path {
            let mut stmt = conn.prepare(
                r#"
                SELECT id, recorded_at_ms, agent_name, scope, project_path,
                       category, title, content, entities, suggested_importance,
                       status, metadata, utility_score, access_count, last_accessed_at_ms
                FROM agent_interactions
                WHERE status = 'active'
                  AND category IN ('constraint', 'user_preference', 'architecture_decision')
                  AND (scope = 'global' OR (scope = 'project' AND project_path = ?1))
                ORDER BY category ASC, recorded_at_ms DESC
                "#,
            )?;
            let rows = stmt.query_map(params![proj], Self::map_agent_interaction_row)?;
            for r in rows {
                results.push(r?);
            }
        } else {
            let mut stmt = conn.prepare(
                r#"
                SELECT id, recorded_at_ms, agent_name, scope, project_path,
                       category, title, content, entities, suggested_importance,
                       status, metadata, utility_score, access_count, last_accessed_at_ms
                FROM agent_interactions
                WHERE status = 'active'
                  AND category IN ('constraint', 'user_preference', 'architecture_decision')
                  AND scope = 'global'
                ORDER BY category ASC, recorded_at_ms DESC
                "#,
            )?;
            let rows = stmt.query_map([], Self::map_agent_interaction_row)?;
            for r in rows {
                results.push(r?);
            }
        }

        Ok(results)
    }

    /// Search agent interactions by query text
    pub fn search_agent_interactions(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<AgentInteractionRow>, MemoryError> {
        let conn = self.conn.lock();
        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            r#"
            SELECT id, recorded_at_ms, agent_name, scope, project_path,
                   category, title, content, entities, suggested_importance,
                   status, metadata, utility_score, access_count, last_accessed_at_ms
            FROM agent_interactions
            WHERE title LIKE ?1 OR content LIKE ?1 OR entities LIKE ?1
            ORDER BY recorded_at_ms DESC
            LIMIT ?2
            "#,
        )?;

        let rows = stmt.query_map(params![pattern, limit as i64], Self::map_agent_interaction_row)?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    /// List recent agent interactions
    pub fn list_recent_agent_interactions(
        &self,
        limit: usize,
    ) -> Result<Vec<AgentInteractionRow>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, recorded_at_ms, agent_name, scope, project_path,
                   category, title, content, entities, suggested_importance,
                   status, metadata, utility_score, access_count, last_accessed_at_ms
            FROM agent_interactions
            ORDER BY recorded_at_ms DESC
            LIMIT ?1
            "#,
        )?;

        let rows = stmt.query_map(params![limit as i64], Self::map_agent_interaction_row)?;
        let mut results = Vec::new();
        for r in rows {
            results.push(r?);
        }
        Ok(results)
    }

    /// Mark an agent interaction as superseded
    pub fn supersede_agent_interaction(&self, id: i64) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE agent_interactions SET status = 'superseded' WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    fn map_agent_interaction_row(row: &rusqlite::Row) -> rusqlite::Result<AgentInteractionRow> {
        let entities_str: String = row.get(8)?;
        let entities: Vec<String> = serde_json::from_str(&entities_str).unwrap_or_default();

        Ok(AgentInteractionRow {
            id: row.get(0)?,
            recorded_at_ms: row.get(1)?,
            agent_name: row.get(2)?,
            scope: row.get(3)?,
            project_path: row.get(4)?,
            category: row.get(5)?,
            title: row.get(6)?,
            content: row.get(7)?,
            entities,
            suggested_importance: row.get(9)?,
            status: row.get(10)?,
            metadata: row.get(11)?,
            utility_score: row.get(12).unwrap_or(1.0),
            access_count: row.get(13).unwrap_or(0),
            last_accessed_at_ms: row.get(14).unwrap_or(0),
        })
    }

    /// Track memory access and optionally update utility score (positive or negative reinforcement)
    pub fn track_memory_access(
        &self,
        target: &str, // "long_term" | "agent"
        id: i64,
        utility_delta: f32,
    ) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        match target {
            "long_term" | "long_term_memory" => {
                conn.execute(
                    r#"
                    UPDATE long_term_memories
                    SET access_count = access_count + 1,
                        last_accessed_at_ms = ?1,
                        utility_score = MAX(0.1, utility_score + ?2),
                        updated_at_ms = ?1
                    WHERE id = ?3
                    "#,
                    params![now_ms, utility_delta, id],
                )?;
            }
            "agent" | "agent_interaction" => {
                conn.execute(
                    r#"
                    UPDATE agent_interactions
                    SET access_count = access_count + 1,
                        last_accessed_at_ms = ?1,
                        utility_score = MAX(0.1, utility_score + ?2)
                    WHERE id = ?3
                    "#,
                    params![now_ms, utility_delta, id],
                )?;
            }
            _ => return Err(MemoryError::Other(format!("Unknown memory target: {}", target))),
        }
        Ok(())
    }

    /// Retrieve active suppression masks ordered by hit count
    pub fn get_active_suppression_masks(&self) -> Result<Vec<SuppressionMask>, MemoryError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, bundle_id, window_title_pattern, reason, hit_count, is_active, created_at_ms, last_hit_at_ms
            FROM suppression_masks
            WHERE is_active = 1
            ORDER BY hit_count DESC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SuppressionMask {
                id: row.get(0)?,
                bundle_id: row.get(1)?,
                window_title_pattern: row.get(2)?,
                reason: row.get(3)?,
                hit_count: row.get(4)?,
                is_active: row.get::<_, i64>(5)? != 0,
                created_at_ms: row.get(6)?,
                last_hit_at_ms: row.get(7)?,
            })
        })?;
        let mut masks = Vec::new();
        for r in rows {
            masks.push(r?);
        }
        Ok(masks)
    }

    /// Record a hit against a suppression mask
    pub fn record_suppression_hit(&self, mask_id: i64) -> Result<(), MemoryError> {
        let conn = self.conn.lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        conn.execute(
            "UPDATE suppression_masks SET hit_count = hit_count + 1, last_hit_at_ms = ?1 WHERE id = ?2",
            params![now, mask_id],
        )?;
        Ok(())
    }

    /// Check if a given application bundle ID and window title matches an active suppression mask
    pub fn is_window_suppressed(&self, bundle_id: &str, window_title: &str) -> Result<bool, MemoryError> {
        let masks = self.get_active_suppression_masks()?;
        for m in masks {
            if m.bundle_id == bundle_id || m.bundle_id == "*" {
                if m.window_title_pattern == "*" || window_title.contains(&m.window_title_pattern) {
                    let _ = self.record_suppression_hit(m.id);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Add or update an autonomous suppression mask
    pub fn add_suppression_mask(
        &self,
        bundle_id: &str,
        window_title_pattern: &str,
        reason: &str,
    ) -> Result<i64, MemoryError> {
        let conn = self.conn.lock();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        conn.execute(
            r#"
            INSERT INTO suppression_masks (
                bundle_id, window_title_pattern, reason, hit_count, is_active, created_at_ms, last_hit_at_ms
            ) VALUES (?1, ?2, ?3, 0, 1, ?4, 0)
            ON CONFLICT(bundle_id, window_title_pattern) DO UPDATE SET
                is_active = 1,
                reason = excluded.reason
            "#,
            params![bundle_id, window_title_pattern, reason, now],
        )?;
        Ok(conn.last_insert_rowid())
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

    /// Aggressive compaction and cold archiving:
    /// 1. Prunes processed L1 captures older than capture_retention_days (48h TTL default).
    /// 2. Prunes unprocessed orphaned L1 captures older than 7 days.
    /// 3. Moves unaccessed L2 episodic memories older than memory_archive_days into memory_archive.db.
    /// 4. Runs SQLite VACUUM to physically shrink memory.db and keep active storage minimal.
    pub fn compact_and_archive(
        &self,
        archive_db_path: &Path,
        capture_retention_days: u32,
        memory_archive_days: u32,
    ) -> Result<CompactionResult, MemoryError> {
        let bytes_before = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let capture_cutoff_ms = now_ms - (capture_retention_days as i64 * 86_400_000);
        let memory_cutoff_ms = now_ms - (memory_archive_days as i64 * 86_400_000);

        let conn = self.conn.lock();

        // 1. Prune processed L1 captures older than TTL
        let mut pruned_captures = conn.execute(
            "DELETE FROM screen_captures WHERE captured_at_ms < ?1 AND processed = 1",
            params![capture_cutoff_ms],
        )?;

        // Also prune unprocessed orphaned captures older than 7 days
        let orphaned_cutoff = now_ms - (7 * 86_400_000);
        let orphaned = conn.execute(
            "DELETE FROM screen_captures WHERE captured_at_ms < ?1 AND processed = 0",
            params![orphaned_cutoff],
        )?;
        pruned_captures += orphaned;

        // 2. Cold Archive L2 Memories
        if let Some(parent) = archive_db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut archive_conn = Connection::open(archive_db_path)?;
        archive_conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS long_term_memories (
                id INTEGER PRIMARY KEY,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                key_entities TEXT NOT NULL,
                importance_score REAL NOT NULL,
                time_window_start_ms INTEGER NOT NULL,
                time_window_end_ms INTEGER NOT NULL,
                source_capture_ids TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 0,
                last_accessed_at_ms INTEGER NOT NULL DEFAULT 0,
                utility_score REAL NOT NULL DEFAULT 1.0,
                archived_at_ms INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_archive_category ON long_term_memories(category);
            CREATE INDEX IF NOT EXISTS idx_archive_archived_at ON long_term_memories(archived_at_ms DESC);
            "#,
        )?;

        struct MemoryArchiveRow {
            id: i64,
            category: String,
            title: String,
            summary: String,
            key_entities: String,
            importance_score: f32,
            time_window_start_ms: i64,
            time_window_end_ms: i64,
            source_capture_ids: String,
            created_at_ms: i64,
            updated_at_ms: i64,
            access_count: i64,
            last_accessed_at_ms: i64,
            utility_score: f32,
        }

        let mut candidate_stmt = conn.prepare(
            r#"
            SELECT id, category, title, summary, key_entities, importance_score,
                   time_window_start_ms, time_window_end_ms, source_capture_ids,
                   created_at_ms, updated_at_ms, access_count, last_accessed_at_ms, utility_score
            FROM long_term_memories
            WHERE time_window_end_ms < ?1 AND (access_count = 0 OR last_accessed_at_ms < ?1)
            "#,
        )?;

        let rows = candidate_stmt
            .query_map(params![memory_cutoff_ms], |r| {
                Ok(MemoryArchiveRow {
                    id: r.get(0)?,
                    category: r.get(1)?,
                    title: r.get(2)?,
                    summary: r.get(3)?,
                    key_entities: r.get(4)?,
                    importance_score: r.get(5)?,
                    time_window_start_ms: r.get(6)?,
                    time_window_end_ms: r.get(7)?,
                    source_capture_ids: r.get(8)?,
                    created_at_ms: r.get(9)?,
                    updated_at_ms: r.get(10)?,
                    access_count: r.get(11)?,
                    last_accessed_at_ms: r.get(12)?,
                    utility_score: r.get(13)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        drop(candidate_stmt);

        let archived_memories = rows.len();
        if !rows.is_empty() {
            let tx = archive_conn.transaction()?;
            for row in &rows {
                tx.execute(
                    r#"
                    INSERT OR REPLACE INTO long_term_memories (
                        id, category, title, summary, key_entities, importance_score,
                        time_window_start_ms, time_window_end_ms, source_capture_ids,
                        created_at_ms, updated_at_ms, access_count, last_accessed_at_ms,
                        utility_score, archived_at_ms
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                    "#,
                    params![
                        row.id,
                        row.category,
                        row.title,
                        row.summary,
                        row.key_entities,
                        row.importance_score,
                        row.time_window_start_ms,
                        row.time_window_end_ms,
                        row.source_capture_ids,
                        row.created_at_ms,
                        row.updated_at_ms,
                        row.access_count,
                        row.last_accessed_at_ms,
                        row.utility_score,
                        now_ms,
                    ],
                )?;
            }
            tx.commit()?;

            // Delete from main database
            for row in &rows {
                conn.execute("DELETE FROM long_term_memories WHERE id = ?1", params![row.id])?;
            }
        }

        // 3. Physical SQLite VACUUM to reclaim deleted pages
        let _ = conn.execute_batch("VACUUM;");

        let bytes_after = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);

        Ok(CompactionResult {
            pruned_captures,
            archived_memories,
            active_db_bytes_before: bytes_before,
            active_db_bytes_after: bytes_after,
        })
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

    #[test]
    fn test_agent_interactions_and_preferences() {
        let db = Database::in_memory().unwrap();

        // 1. Insert global constraint
        let id1 = db.insert_agent_interaction(&NewAgentInteraction {
            agent_name: "claude".into(),
            scope: "global".into(),
            project_path: None,
            category: "constraint".into(),
            title: "Never modify Java build files".into(),
            content: "Intranet Maven repos already globally opted-in; never add allowInsecureProtocol".into(),
            entities: vec!["build.gradle".into(), "Maven".into()],
            suggested_importance: 9.0,
            metadata: None,
        }).unwrap();
        assert_eq!(id1, 1);

        // 2. Insert project-specific constraint for /repo/A
        let id2 = db.insert_agent_interaction(&NewAgentInteraction {
            agent_name: "pi".into(),
            scope: "project".into(),
            project_path: Some("/repo/A".into()),
            category: "constraint".into(),
            title: "Pinned to JDK 8".into(),
            content: "Spring Boot 2.2.7 breaks on JDK 17+; stay on JDK 8".into(),
            entities: vec!["JDK8".into()],
            suggested_importance: 9.5,
            metadata: None,
        }).unwrap();
        assert_eq!(id2, 2);

        // 3. Insert project-specific constraint for /repo/B
        db.insert_agent_interaction(&NewAgentInteraction {
            agent_name: "codex".into(),
            scope: "project".into(),
            project_path: Some("/repo/B".into()),
            category: "constraint".into(),
            title: "Use pnpm only".into(),
            content: "Do not use npm or yarn in repo B".into(),
            entities: vec!["pnpm".into()],
            suggested_importance: 8.0,
            metadata: None,
        }).unwrap();

        // Query preferences for /repo/A: should get global + /repo/A, but NOT /repo/B
        let prefs_a = db.get_active_preferences(Some("/repo/A")).unwrap();
        assert_eq!(prefs_a.len(), 2);
        assert!(prefs_a.iter().any(|p| p.title == "Never modify Java build files"));
        assert!(prefs_a.iter().any(|p| p.title == "Pinned to JDK 8"));
        assert!(!prefs_a.iter().any(|p| p.title == "Use pnpm only"));

        // Query preferences with no project: only global
        let prefs_global = db.get_active_preferences(None).unwrap();
        assert_eq!(prefs_global.len(), 1);
        assert_eq!(prefs_global[0].title, "Never modify Java build files");

        // 4. Test superseding old rule: update "Never modify Java build files"
        let id4 = db.insert_agent_interaction(&NewAgentInteraction {
            agent_name: "thunder".into(),
            scope: "global".into(),
            project_path: None,
            category: "constraint".into(),
            title: "Never modify Java build files".into(),
            content: "Updated: allow modification in test subprojects only".into(),
            entities: vec!["build.gradle".into()],
            suggested_importance: 9.0,
            metadata: None,
        }).unwrap();
        assert_eq!(id4, 4);

        let prefs_updated = db.get_active_preferences(None).unwrap();
        assert_eq!(prefs_updated.len(), 1);
        assert_eq!(prefs_updated[0].id, 4);
        assert!(prefs_updated[0].content.contains("test subprojects only"));
    }

    #[test]
    fn test_state_machine_access_tracking_and_suppression() {
        let db = Database::in_memory().unwrap();

        // 1. Verify default suppression masks seeded
        let masks = db.get_active_suppression_masks().unwrap();
        assert!(!masks.is_empty());
        assert!(masks.iter().any(|m| m.bundle_id == "com.apple.Music"));
        assert!(masks.iter().any(|m| m.bundle_id == "com.spotify.client"));

        // 2. Test is_window_suppressed on default mask
        let suppressed = db.is_window_suppressed("com.spotify.client", "Daily Mix 1").unwrap();
        assert!(suppressed);

        // Not suppressed
        let not_suppressed = db.is_window_suppressed("com.microsoft.VSCode", "src/main.rs").unwrap();
        assert!(!not_suppressed);

        // 3. Add custom suppression mask
        db.add_suppression_mask("com.tencent.xinWeChat", "微信", "Chat window").unwrap();
        let wechat_suppressed = db.is_window_suppressed("com.tencent.xinWeChat", "微信 (3条新消息)").unwrap();
        assert!(wechat_suppressed);

        // 4. Test memory access tracking and utility scoring
        let mem_id = db.insert_long_term_memory(&NewLongTermMemory {
            category: "test".into(),
            title: "Test Memory".into(),
            summary: "Summary".into(),
            key_entities: vec![],
            importance_score: 5.0,
            time_window_start_ms: 100,
            time_window_end_ms: 200,
            source_capture_ids: vec![],
        }).unwrap();

        // Initial utility_score is 1.0, access_count is 0
        let recent = db.list_recent_memories(1).unwrap();
        assert_eq!(recent[0].access_count, 0);
        assert!((recent[0].utility_score - 1.0).abs() < f32::EPSILON);

        // Positive reinforcement access
        db.track_memory_access("long_term", mem_id, 0.5).unwrap();
        let updated = db.list_recent_memories(1).unwrap();
        assert_eq!(updated[0].access_count, 1);
        assert!((updated[0].utility_score - 1.5).abs() < f32::EPSILON);
        assert!(updated[0].last_accessed_at_ms > 0);
    }

    #[test]
    fn test_compaction_and_cold_archiving() {
        let temp_dir = tempfile::tempdir().unwrap();
        let main_db_path = temp_dir.path().join("memory.db");
        let archive_db_path = temp_dir.path().join("memory_archive.db");

        let db = Database::open(&main_db_path).unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        // 1. Insert old processed capture (3 days ago > 48h TTL)
        let old_capture_ms = now_ms - (3 * 86_400_000);
        let id1 = db.insert_capture(&ScreenCaptureRecord {
            timestamp_ms: old_capture_ms,
            app_name: "OldApp".into(),
            bundle_id: "com.old".into(),
            window_title: "OldWin".into(),
            full_text: "old content".into(),
            avg_confidence: 0.9,
            content_hash: "hash_old".into(),
            metadata_json: "[]".into(),
        }).unwrap();
        db.mark_captures_processed(&[id1]).unwrap();

        // 2. Insert recent processed capture (1 hour ago < 48h TTL)
        let recent_capture_ms = now_ms - 3_600_000;
        let id2 = db.insert_capture(&ScreenCaptureRecord {
            timestamp_ms: recent_capture_ms,
            app_name: "RecentApp".into(),
            bundle_id: "com.recent".into(),
            window_title: "RecentWin".into(),
            full_text: "recent content".into(),
            avg_confidence: 0.9,
            content_hash: "hash_recent".into(),
            metadata_json: "[]".into(),
        }).unwrap();
        db.mark_captures_processed(&[id2]).unwrap();

        // 3. Insert old inactive L2 memory (40 days ago, never accessed)
        let old_mem_end_ms = now_ms - (40 * 86_400_000);
        let mem1 = db.insert_long_term_memory(&NewLongTermMemory {
            category: "task".into(),
            title: "Old Inactive Task".into(),
            summary: "Archivable task".into(),
            key_entities: vec!["OldEntity".into()],
            importance_score: 5.0,
            time_window_start_ms: old_mem_end_ms - 1000,
            time_window_end_ms: old_mem_end_ms,
            source_capture_ids: vec![],
        }).unwrap();

        // 4. Insert old ACTIVE L2 memory (40 days ago, accessed recently)
        let mem2 = db.insert_long_term_memory(&NewLongTermMemory {
            category: "task".into(),
            title: "Old Active Task".into(),
            summary: "Frequently retrieved task".into(),
            key_entities: vec!["ActiveEntity".into()],
            importance_score: 8.0,
            time_window_start_ms: old_mem_end_ms - 1000,
            time_window_end_ms: old_mem_end_ms,
            source_capture_ids: vec![],
        }).unwrap();
        db.track_memory_access("long_term", mem2, 1.0).unwrap();

        // 5. Insert recent L2 memory (5 days ago < 30 days)
        let recent_mem_end_ms = now_ms - (5 * 86_400_000);
        let mem3 = db.insert_long_term_memory(&NewLongTermMemory {
            category: "task".into(),
            title: "Recent Task".into(),
            summary: "Recent task summary".into(),
            key_entities: vec!["RecentEntity".into()],
            importance_score: 7.0,
            time_window_start_ms: recent_mem_end_ms - 1000,
            time_window_end_ms: recent_mem_end_ms,
            source_capture_ids: vec![],
        }).unwrap();

        // Run compact_and_archive: retention 2 days, archive 30 days
        let res = db.compact_and_archive(&archive_db_path, 2, 30).unwrap();

        assert_eq!(res.pruned_captures, 1);
        assert_eq!(res.archived_memories, 1);

        // Verify main DB: mem1 should be deleted; mem2 and mem3 should remain
        let remaining = db.list_recent_memories(10).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|m| m.id == mem2));
        assert!(remaining.iter().any(|m| m.id == mem3));
        assert!(!remaining.iter().any(|m| m.id == mem1));

        // Verify archive DB: contains mem1
        let archive_conn = Connection::open(&archive_db_path).unwrap();
        let mut stmt = archive_conn
            .prepare("SELECT title, access_count, archived_at_ms FROM long_term_memories WHERE id = ?1")
            .unwrap();
        let row = stmt
            .query_row(params![mem1], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .unwrap();
        assert_eq!(row.0, "Old Inactive Task");
        assert_eq!(row.1, 0);
        assert!(row.2 > 0);
    }
}
