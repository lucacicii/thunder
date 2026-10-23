use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Path to thunder-memory SQLite database
    pub db_path: PathBuf,

    /// Interval in seconds between screen captures (default: 5)
    pub capture_interval_secs: u64,

    /// Interval in seconds between periodic small-model synthesis runs (default: 300 = 5m)
    pub synthesis_interval_secs: u64,

    /// Maximum unprocessed captures to process in a single synthesis cycle (default: 50)
    pub synthesis_batch_size: usize,

    /// Path to agent-resume-panel desktop database containing workbench_composer_sends
    pub composer_db_path: PathBuf,

    /// Local or remote OpenAI-compatible LLM endpoint URL (e.g. http://localhost:11434/v1)
    pub llm_endpoint: Option<String>,

    /// Small model name to use for memory synthesis (e.g. qwen2.5-coder, llama3.2, etc.)
    pub llm_model: String,

    /// Optional API key for LLM endpoint
    pub llm_api_key: Option<String>,

    /// Minimum average OCR confidence (0.0 .. 1.0) required to record a screen capture
    pub min_confidence: f32,

    /// Retention period for raw screen captures in days (default: 2 days / 48h TTL).
    pub raw_capture_retention_days: u32,

    /// Cold archive threshold for inactive long-term memories in days (default: 30).
    pub memory_archive_days: u32,

    /// Minimum interval in seconds between captures during active typing / UI updates (default: 2)
    pub min_capture_interval_secs: u64,

    /// Maximum interval in seconds between captures during static/idle screens (default: 60)
    pub max_capture_interval_secs: u64,

    /// Path to cold archive SQLite database (default: ~/.thunder/memory_archive.db)
    pub archive_db_path: PathBuf,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let thunder_dir = PathBuf::from(&home).join(".thunder");
        let default_composer_db = PathBuf::from(&home)
            .join(".agent-resume-panel")
            .join(".desktop")
            .join("desktop.db");

        Self {
            db_path: thunder_dir.join("memory.db"),
            capture_interval_secs: 5,
            synthesis_interval_secs: 300,
            synthesis_batch_size: 50,
            composer_db_path: default_composer_db,
            llm_endpoint: None,
            llm_model: "qwen2.5-coder".to_string(),
            llm_api_key: None,
            min_confidence: 0.1,
            raw_capture_retention_days: 2,
            memory_archive_days: 30,
            min_capture_interval_secs: 2,
            max_capture_interval_secs: 60,
            archive_db_path: thunder_dir.join("memory_archive.db"),
        }
    }
}

impl MemoryConfig {
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// Resolve actual path for composer DB (handles both .desktop/desktop.db and desktop/desktop.db)
    pub fn resolve_composer_db_path(&self) -> Option<PathBuf> {
        if self.composer_db_path.exists() {
            return Some(self.composer_db_path.clone());
        }

        // Try fallback without hidden dot: ~/.agent-resume-panel/desktop/desktop.db
        if let Some(home) = std::env::var_os("HOME") {
            let alt_path = PathBuf::from(home)
                .join(".agent-resume-panel")
                .join("desktop")
                .join("desktop.db");
            if alt_path.exists() {
                return Some(alt_path);
            }
        }

        None
    }
}
