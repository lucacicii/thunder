use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thunder_agent_loop::{AgentRunResult, FinishReason};
use tokio::fs;

/// On-disk record of one finished A unit. B owns this; A does not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRun {
    pub run_id: String,
    pub agent_id: String,
    pub role: String,
    pub finish_reason: FinishReason,
    pub final_content: Option<String>,
    pub total_turns: usize,
    pub messages: Vec<thunder_agent_loop::ChatMessage>,
}

#[derive(Debug, Clone)]
pub struct RunStore {
    root: PathBuf,
}

impl RunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn run_dir(&self, run_id: &str) -> PathBuf {
        self.root.join(run_id)
    }

    pub async fn save(
        &self,
        run_id: &str,
        role: &str,
        result: &AgentRunResult,
    ) -> Result<PathBuf, std::io::Error> {
        let dir = self.run_dir(run_id);
        fs::create_dir_all(&dir).await?;

        let record = StoredRun {
            run_id: run_id.to_string(),
            agent_id: result.agent_id.clone(),
            role: role.to_string(),
            finish_reason: result.finish_reason.clone(),
            final_content: result.final_content.clone(),
            total_turns: result.stats.total_turns,
            messages: result.messages.clone(),
        };

        let path = dir.join(format!("{}.json", result.agent_id));
        let json = serde_json::to_string_pretty(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::write(&path, json).await?;
        Ok(path)
    }

    pub async fn load(&self, run_id: &str, agent_id: &str) -> Result<StoredRun, std::io::Error> {
        let path = self.run_dir(run_id).join(format!("{agent_id}.json"));
        let bytes = fs::read(&path).await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}
