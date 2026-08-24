use crate::core::utf8::{safe_slice_from, safe_slice_to};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::fs;

/// Resolves the standard user home directory for thunder agent temporary files (`~/.thunder/scratchpad`)
pub fn default_thunder_scratchpad_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".thunder").join("scratchpad")
    } else if let Ok(userprofile) = std::env::var("USERPROFILE") {
        PathBuf::from(userprofile).join(".thunder").join("scratchpad")
    } else {
        std::env::temp_dir().join(".thunder").join("scratchpad")
    }
}

/// Record of an oversized tool output persisted to the scratchpad directory
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

/// Manifest tracking all persisted artifacts for a specific agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub session_id: String,
    pub scratchpad_dir: PathBuf,
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone)]
pub struct ScratchpadConfig {
    /// Directory where large outputs are saved (default: `~/.thunder/scratchpad`)
    pub base_dir: PathBuf,
    /// Threshold in bytes above which outputs are persisted to disk (default: 16 KB)
    pub threshold_bytes: usize,
    /// Maximum bytes to include in the preview head/tail (default: 1024 bytes each)
    pub preview_bytes: usize,
    /// Automatically delete scratchpad directory when session concludes (default: true)
    pub auto_cleanup: bool,
}

impl Default for ScratchpadConfig {
    fn default() -> Self {
        Self {
            base_dir: default_thunder_scratchpad_dir(),
            threshold_bytes: 16 * 1024, // 16 KB
            preview_bytes: 1024,
            auto_cleanup: true,
        }
    }
}

/// Thread-safe manager for file-backed large tool output persistence and manifest tracking
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

    pub fn with_default_session(config: ScratchpadConfig) -> Self {
        let sid = format!("sess_{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
        Self::new(sid, config)
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

    /// Processes a tool execution output:
    /// - If size <= threshold_bytes, returns original output string.
    /// - If size > threshold_bytes, asynchronously persists full output to disk and returns a handle message.
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

        // Ensure session directory exists
        if !self.session_dir.exists() {
            fs::create_dir_all(&self.session_dir).await?;
        }

        let idx = self.counter.fetch_add(1, Ordering::SeqCst);
        let filename = format!("turn_{:02}_{}_{}.log", turn, tool_name, idx);
        let file_path = self.session_dir.join(&filename);

        // 1. Write full lossless output to disk
        fs::write(&file_path, output.as_bytes()).await?;

        let total_lines = output.lines().count();

        // 2. Generate safe preview (head + tail)
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

        let artifact_id = format!("art_turn{:02}_{}_{}", turn, tool_name, idx);
        let artifact = Artifact {
            id: artifact_id.clone(),
            turn,
            tool_name: tool_name.to_string(),
            file_path: file_path.clone(),
            total_bytes,
            total_lines,
            preview: preview.clone(),
        };

        // Record in manifest
        {
            let mut guard = self.manifest.write();
            guard.artifacts.push(artifact);
        }

        // 3. Format structured handle message for LLM context
        let path_str = file_path.to_string_lossy();
        let handle_msg = format!(
            "[Large Output Saved to Disk]\n\
            - File Path: {}\n\
            - Size: {} bytes ({} lines)\n\
            - Artifact ID: {}\n\
            - Preview:\n{}\n\n\
            Tip: You can use `read_file(\"{}\", offset, limit)` or `bash(\"grep ... {}\")` to inspect specific sections.",
            path_str, total_bytes, total_lines, artifact_id, preview, path_str, path_str
        );

        Ok(handle_msg)
    }

    /// Returns a snapshot of all artifacts persisted in the current session
    pub fn get_manifest(&self) -> ArtifactManifest {
        self.manifest.read().clone()
    }

    /// Returns a compact summary string of all available artifacts for inclusion in state digests
    pub fn format_artifacts_summary(&self) -> String {
        let manifest = self.manifest.read();
        if manifest.artifacts.is_empty() {
            return String::new();
        }

        let mut out = String::from("【Persisted Artifacts Available for On-Demand Search】:\n");
        for art in &manifest.artifacts {
            out.push_str(&format!(
                "- `{}` (Tool: '{}', {} bytes, {} lines)\n",
                art.file_path.display(),
                art.tool_name,
                art.total_bytes,
                art.total_lines
            ));
        }
        out
    }

    /// Removes scratchpad temporary files
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

    #[tokio::test]
    async fn test_scratchpad_threshold_and_persistence() {
        let temp_dir = std::env::temp_dir().join(format!("thunder_test_scratch_{}", std::process::id()));
        let config = ScratchpadConfig {
            base_dir: temp_dir.clone(),
            threshold_bytes: 500, // 500 bytes threshold
            preview_bytes: 100,
            auto_cleanup: true,
        };

        let manager = ScratchpadManager::new("test_session", config);

        // 1. Small output: untouched
        let small = "Hello small output";
        let res_small = manager.process_tool_output("echo", 1, small.to_string()).await.unwrap();
        assert_eq!(res_small, small);
        assert_eq!(manager.get_manifest().artifacts.len(), 0);

        // 2. Large output: saved to disk
        let large = "Line of big log output text for testing.\n".repeat(50); // ~2000 bytes
        let res_large = manager.process_tool_output("bash", 2, large.clone()).await.unwrap();

        assert!(res_large.contains("[Large Output Saved to Disk]"));
        assert!(res_large.contains("turn_02_bash_1.log"));

        let manifest = manager.get_manifest();
        assert_eq!(manifest.artifacts.len(), 1);
        let art = &manifest.artifacts[0];
        assert_eq!(art.tool_name, "bash");
        assert_eq!(art.turn, 2);
        assert!(art.file_path.exists());

        // Verify content on disk is lossless
        let on_disk = fs::read_to_string(&art.file_path).await.unwrap();
        assert_eq!(on_disk, large);

        // 3. Artifact summary formatting
        let summary = manager.format_artifacts_summary();
        assert!(summary.contains("turn_02_bash_1.log"));
        assert!(summary.contains("bash"));

        // 4. Cleanup
        manager.cleanup().await.unwrap();
        assert!(!manager.session_dir().exists());
    }
}
