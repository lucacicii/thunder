use crate::flood_guard::{safe_slice_from, safe_slice_to};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::fs;

pub fn default_thunder_scratchpad_dir() -> PathBuf {
    thunder_protocol::paths::thunder_subdir("scratchpad")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub turn: usize,
    pub tool_name: String,
    pub file_path: PathBuf,
    pub total_bytes: usize,
    pub total_lines: usize,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub session_id: String,
    pub scratchpad_dir: PathBuf,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadConfig {
    pub base_dir: PathBuf,
    pub threshold_bytes: usize,
    pub preview_bytes: usize,
    pub auto_cleanup: bool,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            base_dir: default_thunder_scratchpad_dir(),
            threshold_bytes: 16 * 1024,
            preview_bytes: 1024,
            auto_cleanup: false,
        }
    }
}

#[derive(Clone)]
pub struct ScratchpadManager {
    config: ScratchpadConfig,
    session_id: String,
    session_dir: PathBuf,
    counter: Arc<AtomicUsize>,
    manifest: Arc<RwLock<ArtifactManifest>>,
}

impl ScratchpadManager {
    pub fn new(session_id: impl Into<String>, config: ScratchpadConfig) -> Self {
        let sid = session_id.into();
        let session_dir = config.base_dir.join(&sid);

        let manifest = ArtifactManifest {
            session_id: sid.clone(),
            scratchpad_dir: session_dir.clone(),
            artifacts: Vec::new(),
        };

        Self {
            config,
            session_id: sid,
            session_dir,
            counter: Arc::new(AtomicUsize::new(1)),
            manifest: Arc::new(RwLock::new(manifest)),
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn threshold_bytes(&self) -> usize {
        self.config.threshold_bytes
    }

    pub async fn process_tool_output(
        &self,
        tool_name: &str,
        turn: usize,
        output: String,
    ) -> Result<String, std::io::Error> {
        let total_bytes = output.len();
        if total_bytes <= self.config.threshold_bytes {
            return Ok(output);
        }

        if !self.session_dir.exists() {
            fs::create_dir_all(&self.session_dir).await?;
        }

        let idx = self.counter.fetch_add(1, Ordering::SeqCst);
        let clean_tool = thunder_protocol::sanitize_identifier(tool_name);
        let filename = format!("turn_{:02}_{}_{}.log", turn, clean_tool, idx);
        let file_path = self.session_dir.join(&filename);

        fs::write(&file_path, output.as_bytes()).await?;

        let total_lines = output.lines().count();
        let preview_len = self.config.preview_bytes;
        let head = safe_slice_to(&output, preview_len);
        let tail = safe_slice_from(&output, output.len().saturating_sub(preview_len));

        let preview = format!(
            "{}\n\n[... {} lines / {} bytes stored in file ...]\n\n{}",
            head,
            total_lines.saturating_sub(20),
            total_bytes.saturating_sub(head.len() + tail.len()),
            tail
        );

        let artifact_id = format!("art_turn{:02}_{}_{}", turn, clean_tool, idx);
        let artifact = Artifact {
            id: artifact_id.clone(),
            turn,
            tool_name: tool_name.to_string(),
            file_path: file_path.clone(),
            total_bytes,
            total_lines,
            preview: preview.clone(),
        };

        self.manifest.write().artifacts.push(artifact);

        let path_str = file_path.to_string_lossy();
        Ok(format!(
            "[Large Output Saved to Disk]\n\
            - File Path: {}\n\
            - Size: {} bytes ({} lines)\n\
            - Artifact ID: {}\n\
            - Preview:\n{}\n\n\
            Tip: You can use `core__read_file(\"{}\", offset, limit)` to inspect specific sections.",
            path_str, total_bytes, total_lines, artifact_id, preview, path_str
        ))
    }

    pub fn get_manifest(&self) -> ArtifactManifest {
        self.manifest.read().clone()
    }

    pub async fn cleanup(&self) -> Result<(), std::io::Error> {
        if self.session_dir.exists() {
            fs::remove_dir_all(&self.session_dir).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn scratchpad_persists_oversized_output_with_head_and_tail_preview() {
        let dir = tempdir().unwrap();
        let cfg = ScratchpadConfig {
            base_dir: dir.path().to_path_buf(),
            threshold_bytes: 200,
            preview_bytes: 50,
            auto_cleanup: true,
        };
        let mgr = ScratchpadManager::new("sess-1", cfg);

        let small = mgr
            .process_tool_output("bash", 1, "short".into())
            .await
            .unwrap();
        assert_eq!(small, "short");

        let large = format!("START_LINE\n{}\nEND_LINE", "x".repeat(500));
        let handled = mgr.process_tool_output("bash", 2, large).await.unwrap();
        assert!(handled.contains("[Large Output Saved to Disk]"));
        assert!(handled.contains("START_LINE"));
        assert!(handled.contains("END_LINE"));
        assert_eq!(mgr.get_manifest().artifacts.len(), 1);
    }
}
