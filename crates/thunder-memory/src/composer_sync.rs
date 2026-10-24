use crate::config::MemoryConfig;
use crate::db::{ComposerTrainingSample, Database};
use crate::error::MemoryError;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use tracing::{debug, info, warn};

pub struct ComposerSyncEngine {
    db: Database,
    config: MemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiTrainingItem {
    pub messages: Vec<OpenAiMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlpacaTrainingItem {
    pub instruction: String,
    pub input: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareGptTrainingItem {
    pub conversations: Vec<ShareGptMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareGptMessage {
    pub from: String,
    pub value: String,
}

impl ComposerSyncEngine {
    pub fn new(db: Database, config: MemoryConfig) -> Self {
        Self { db, config }
    }

    /// Sync new composer sends from desktop.db into local composer_training_samples table
    pub fn sync_composer_sends(&self) -> Result<usize, MemoryError> {
        let source_path = match self.config.resolve_composer_db_path() {
            Some(p) => p,
            None => {
                warn!(
                    "Composer source database not found at {:?}. Skipping sync.",
                    self.config.composer_db_path
                );
                return Ok(0);
            }
        };

        info!("Syncing workbench_composer_sends from {:?}", source_path);

        let source_conn = Connection::open_with_flags(
            &source_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        let last_watermark_ms = self
            .db
            .get_meta_i64("composer_sync_watermark_ms")?
            .unwrap_or(0);

        let mut stmt = source_conn.prepare(
            r#"
            SELECT id, created_at_ms, pane_key, project_path, session_key, provider, agent_session_id, text
            FROM workbench_composer_sends
            WHERE created_at_ms > ?1
            ORDER BY created_at_ms ASC
            LIMIT 5000
            "#,
        )?;

        let mut max_synced_ms = last_watermark_ms;
        let mut count = 0;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let rows = stmt.query_map(params![last_watermark_ms], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;

        for r in rows {
            let (id, created_at_ms, pane_key, project_path, _session_key, provider, agent_session_id, text) = r?;

            if created_at_ms > max_synced_ms {
                max_synced_ms = created_at_ms;
            }

            let (intent, quality, entities, prompt) =
                Self::analyze_composer_send(&text, &project_path);

            let sample = ComposerTrainingSample {
                id,
                created_at_ms,
                project_path,
                pane_key,
                provider,
                agent_session_id,
                raw_text: text,
                intent_category: intent,
                extracted_entities: entities,
                quality_score: quality,
                training_prompt: prompt,
                synced_at_ms: now_ms,
            };

            self.db.insert_composer_sample(&sample)?;
            count += 1;
        }

        if count > 0 {
            self.db
                .set_meta("composer_sync_watermark_ms", &max_synced_ms.to_string())?;
            info!(
                "Successfully synced {} new composer sends (new watermark: {})",
                count, max_synced_ms
            );
        } else {
            debug!("No new composer sends found since watermark {}", last_watermark_ms);
        }

        Ok(count)
    }

    /// Analyze a user composer send: classify intent, compute training quality, extract entities, format prompt
    pub fn analyze_composer_send(text: &str, project_path: &str) -> (String, f32, Vec<String>, String) {
        let trimmed = text.trim();
        let lower = trimmed.to_lowercase();

        // 1. Quality evaluation
        let mut quality: f32 = 0.5;
        let char_count = trimmed.chars().count();

        if char_count < 3 {
            quality = 0.1;
        } else if char_count < 10 {
            quality = 0.3;
        } else if char_count > 50 {
            quality += 0.2;
        }

        if trimmed.contains("```") || trimmed.contains("fn ") || trimmed.contains("def ") || trimmed.contains("class ") {
            quality += 0.2;
        }
        if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains(".rs") || trimmed.contains(".ts") {
            quality += 0.1;
        }

        // 2. Intent classification
        let intent = if lower.starts_with("git ") || lower.contains("commit") || lower.contains("push") || lower.contains("rebase") {
            "git_ops".to_string()
        } else if lower.starts_with("不要") || lower.starts_with("别") || lower.starts_with("禁止") || lower.contains("never") {
            "constraint_steer".to_string()
        } else if lower.ends_with('?') || lower.ends_with('？') || lower.contains("如何") || lower.contains("怎么") || lower.contains("是什么") {
            "architectural_inquiry".to_string()
        } else if lower.contains("error") || lower.contains("bug") || lower.contains("修复") || lower.contains("报错") || lower.contains("fix") {
            "bug_fix".to_string()
        } else if lower.contains("重构") || lower.contains("优化") || lower.contains("refactor") {
            "refactoring".to_string()
        } else if lower == "执行" || lower == "继续" || lower == "确认" || lower == "yes" || lower == "ok" {
            "action_execution".to_string()
        } else {
            "feature_development".to_string()
        };

        // 3. Extract entities (paths, files, tools)
        let mut entities = Vec::new();
        for word in trimmed.split_whitespace() {
            let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_' && c != '-');
            if (clean.contains('/') || clean.contains('.')) && clean.len() > 3 {
                entities.push(clean.to_string());
            }
        }

        // 4. Normalized training instruction prompt
        let formatted_prompt = format!(
            "[Project Context: {}]\n[User Instruction]: {}",
            project_path,
            trimmed
        );

        if quality > 1.0 {
            quality = 1.0;
        }

        (intent, quality, entities, formatted_prompt)
    }

    /// Export training samples to a JSONL file
    pub fn export_dataset(
        &self,
        output_path: &Path,
        format: &str,
        min_quality: f32,
        limit: Option<usize>,
    ) -> Result<usize, MemoryError> {
        let samples = self.db.get_training_samples(min_quality, limit)?;
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let file = File::create(output_path)?;
        let mut writer = BufWriter::new(file);
        let mut written = 0;

        for s in &samples {
            let line = match format.to_lowercase().as_str() {
                "alpaca" => {
                    let item = AlpacaTrainingItem {
                        instruction: s.training_prompt.clone(),
                        input: format!("Target project path: {}", s.project_path),
                        output: String::new(),
                    };
                    serde_json::to_string(&item)?
                }
                "sharegpt" => {
                    let item = ShareGptTrainingItem {
                        conversations: vec![
                            ShareGptMessage {
                                from: "system".into(),
                                value: "You are an expert AI software development assistant.".into(),
                            },
                            ShareGptMessage {
                                from: "human".into(),
                                value: s.training_prompt.clone(),
                            },
                        ],
                    };
                    serde_json::to_string(&item)?
                }
                _ => {
                    // Default: OpenAI format
                    let item = OpenAiTrainingItem {
                        messages: vec![
                            OpenAiMessage {
                                role: "system".into(),
                                content: "You are an expert AI software engineer assistant.".into(),
                            },
                            OpenAiMessage {
                                role: "user".into(),
                                content: s.training_prompt.clone(),
                            },
                        ],
                    };
                    serde_json::to_string(&item)?
                }
            };

            writeln!(writer, "{}", line)?;
            written += 1;
        }

        writer.flush()?;
        info!(
            "Exported {} training samples to {:?} (format: {}, min_quality: {:.2})",
            written, output_path, format, min_quality
        );
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_composer_send() {
        let (intent, quality, entities, prompt) = ComposerSyncEngine::analyze_composer_send(
            "你看下 /Users/lucas/wz/GitHub/thunder/Cargo.toml 中的 workspace 配置 是怎么设置的？",
            "/Users/lucas/wz/GitHub/thunder",
        );

        assert_eq!(intent, "architectural_inquiry");
        assert!(quality >= 0.7);
        assert!(entities.iter().any(|e| e.contains("Cargo.toml")));
        assert!(prompt.contains("[Project Context:"));
    }

    #[test]
    fn test_analyze_short_instruction() {
        let (intent, quality, _, _) = ComposerSyncEngine::analyze_composer_send("执行", "/tmp");
        assert_eq!(intent, "action_execution");
        assert!(quality <= 0.3);
    }
}
