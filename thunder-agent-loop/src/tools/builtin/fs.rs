use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Default)]
pub struct ReadFileTool {
    default_cwd: Option<PathBuf>,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait]
impl AgentTool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "read_file",
            "Read file contents with optional line offset and limit.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed)."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read."
                    }
                },
                "required": ["path"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'path' parameter")?;

        let p = Path::new(path_str);
        let target_path = if p.is_relative() {
            if let Some(ref cwd) = self.default_cwd {
                cwd.join(p)
            } else {
                p.to_path_buf()
            }
        } else {
            p.to_path_buf()
        };

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        if let Ok(meta) = fs::metadata(&target_path).await {
            let max_bytes = 10 * 1024 * 1024; // 10 MB ceiling
            if meta.len() > max_bytes && limit.is_none() {
                return Err(format!(
                    "File '{}' ({:.2} MB) exceeds maximum allowed single read limit of 10 MB. Please specify 'limit' or 'offset' to read in chunks.",
                    target_path.display(),
                    meta.len() as f64 / (1024.0 * 1024.0)
                ));
            }
        }

        let content = fs::read_to_string(&target_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {}", target_path.display(), e))?;

        if offset <= 1 && limit.is_none() {
            return Ok(content);
        }

        // Stream the requested slice without materializing every line.
        let start = offset.saturating_sub(1);
        let take = limit.unwrap_or(usize::MAX);
        let mut out = String::new();
        for line in content.lines().skip(start).take(take) {
            out.push_str(line);
            out.push('\n');
        }
        Ok(out.trim_end_matches('\n').to_string())
    }
}

#[derive(Default)]
pub struct WriteFileTool {
    default_cwd: Option<PathBuf>,
}

impl WriteFileTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_cwd(mut self, cwd: PathBuf) -> Self {
        self.default_cwd = Some(cwd);
        self
    }
}

#[async_trait]
impl AgentTool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "write_file",
            "Write content to a file on disk.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path."
                    },
                    "content": {
                        "type": "string",
                        "description": "File content string."
                    }
                },
                "required": ["path", "content"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let path_str = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'path' parameter")?;
        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'content' parameter")?;

        let p = Path::new(path_str);
        let path = if p.is_relative() {
            if let Some(ref cwd) = self.default_cwd {
                cwd.join(p)
            } else {
                p.to_path_buf()
            }
        } else {
            p.to_path_buf()
        };

        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Failed to create parent directory: {}", e))?;

        // Atomic write via staging temp file + fsync + atomic rename
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("file");
        let temp_path = parent.join(format!(".{}_{}_{}.tmp", file_name, pid, nanos));

        let original_permissions = if path.exists() {
            fs::metadata(&path).await.ok().map(|m| m.permissions())
        } else {
            None
        };

        // Write and sync bytes
        {
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .await
                .map_err(|e| {
                    format!(
                        "Failed to create staging file '{}': {}",
                        temp_path.display(),
                        e
                    )
                })?;

            use tokio::io::AsyncWriteExt;
            file.write_all(content.as_bytes()).await.map_err(|e| {
                let _ = std::fs::remove_file(&temp_path);
                format!("Failed writing to staging file: {}", e)
            })?;

            file.sync_all().await.map_err(|e| {
                let _ = std::fs::remove_file(&temp_path);
                format!("Failed syncing staging file: {}", e)
            })?;
        }

        if let Some(perm) = original_permissions {
            let _ = tokio::fs::set_permissions(&temp_path, perm).await;
        }

        if let Err(e) = tokio::fs::rename(&temp_path, &path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(format!(
                "Failed to atomically rename to '{}': {}",
                path.display(),
                e
            ));
        }

        Ok(format!(
            "Successfully wrote {} bytes to {}",
            content.len(),
            path.display()
        ))
    }
}
