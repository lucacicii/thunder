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

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
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

        let content = fs::read_to_string(&target_path)
            .await
            .map_err(|e| format!("Failed to read file '{}': {}", target_path.display(), e))?;

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);

        if offset <= 1 && limit.is_none() {
            return Ok(content);
        }

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.saturating_sub(1);
        let end = match limit {
            Some(l) => (start + l).min(lines.len()),
            None => lines.len(),
        };

        if start >= lines.len() {
            return Ok(String::new());
        }

        Ok(lines[start..end].join("\n"))
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

    async fn execute(&self, args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
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

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create parent directory: {}", e))?;
        }

        fs::write(&path, content)
            .await
            .map_err(|e| format!("Failed to write file '{}': {}", path.display(), e))?;

        Ok(format!("Successfully wrote {} bytes to {}", content.len(), path.display()))
    }
}
