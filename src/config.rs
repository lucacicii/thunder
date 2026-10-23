use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
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

    /// OpenAI-compatible LLM endpoint (e.g. Ollama's http://127.0.0.1:11434/v1).
    ///
    /// Thunder-memory runs no inference itself: without an endpoint the synthesis
    /// tier falls back to the deterministic rule extractor.
    pub llm_endpoint: Option<String>,

    /// Model name served by `llm_endpoint` (e.g. `qwen2.5-coder:0.5b` on Ollama)
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
            llm_model: "qwen2.5-coder:0.5b".to_string(),
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
    /// Load configuration from ~/.thunder/memory.toml or ~/.thunder/config.toml, falling back to defaults
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let thunder_dir = PathBuf::from(&home).join(".thunder");

        // 1. Try ~/.thunder/memory.toml
        let memory_toml = thunder_dir.join("memory.toml");
        if memory_toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&memory_toml) {
                if let Ok(cfg) = toml::from_str::<MemoryConfig>(&content) {
                    return cfg;
                }
            }
        }

        // 2. Try ~/.thunder/config.toml under [memory] table
        let config_toml = thunder_dir.join("config.toml");
        if config_toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_toml) {
                if let Ok(parsed) = toml::from_str::<toml::Value>(&content) {
                    if let Some(mem_val) = parsed.get("memory") {
                        if let Ok(cfg) = mem_val.clone().try_into::<MemoryConfig>() {
                            return cfg;
                        }
                    }
                }
            }
        }

        // 3. Fallback to default + env vars
        let mut cfg = Self::default();

        if let Ok(endpoint) = std::env::var("THUNDER_MEMORY_LLM_ENDPOINT") {
            cfg.llm_endpoint = Some(endpoint);
        }
        if let Ok(model) = std::env::var("THUNDER_MEMORY_LLM_MODEL") {
            cfg.llm_model = model;
        }
        if let Ok(key) = std::env::var("THUNDER_MEMORY_LLM_KEY") {
            cfg.llm_api_key = Some(key);
        }

        cfg
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_toml_parsing() {
        let toml_str = r#"
            capture_interval_secs = 10
            min_capture_interval_secs = 3
            llm_endpoint = "http://localhost:11434/v1"
            llm_model = "qwen2.5-coder:7b"
        "#;

        let cfg: MemoryConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.capture_interval_secs, 10);
        assert_eq!(cfg.min_capture_interval_secs, 3);
        assert_eq!(cfg.llm_endpoint, Some("http://localhost:11434/v1".to_string()));
        assert_eq!(cfg.llm_model, "qwen2.5-coder:7b");
        // Defaults preserved
        assert_eq!(cfg.raw_capture_retention_days, 2);
        assert_eq!(cfg.memory_archive_days, 30);
    }
}
