use crate::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub struct BashTool {
    max_buffer_bytes: usize,
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new(1024 * 1024)
    }
}

impl BashTool {
    pub fn new(max_buffer_bytes: usize) -> Self {
        Self { max_buffer_bytes }
    }
}

#[async_trait]
impl AgentTool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "bash",
            "Execute a bash shell command and capture standard output and standard error.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute."
                    }
                },
                "required": ["command"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value, ctx: &ToolExecutionContext) -> Result<String, String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing required parameter 'command'".to_string())?;

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdin(Stdio::null()) // Prevent interactive hanging on user stdin prompts
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Injected non-interactive safe environment variables
            .env("DEBIAN_FRONTEND", "noninteractive")
            .env("CI", "true")
            .env("TERM", "dumb")
            .env("PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0")
            .spawn()
            .map_err(|e| format!("Failed to spawn bash process: {}", e))?;

        let mut stdout_pipe = child.stdout.take().ok_or("Failed to capture stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("Failed to capture stderr")?;

        let cancel_token = ctx.cancellation_token.clone();

        // Read both pipes concurrently in independent tasks so neither stream's EOF
        // causes premature termination and data loss on the other.
        let max_buf = self.max_buffer_bytes;
        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stdout_pipe.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.len() < max_buf {
                            buf.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(e) => return Err(format!("Stdout read error: {}", e)),
                }
            }
            Ok::<Vec<u8>, String>(buf)
        });

        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match stderr_pipe.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if buf.len() < max_buf {
                            buf.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(e) => return Err(format!("Stderr read error: {}", e)),
                }
            }
            Ok::<Vec<u8>, String>(buf)
        });

        let result = tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                // Reap the killed child to prevent zombie processes
                let _ = child.wait().await;
                Err("Process killed by cancellation signal".to_string())
            }
            res = async {
                let status = child.wait().await.map_err(|e| format!("Wait child error: {}", e))?;
                // Drain remaining pipe data after process exit
                let stdout_buf = stdout_task.await.map_err(|e| format!("Stdout task join error: {}", e))??;
                let stderr_buf = stderr_task.await.map_err(|e| format!("Stderr task join error: {}", e))??;
                Ok((status, stdout_buf, stderr_buf))
            } => {
                res
            }
        };

        let (status, stdout_buf, stderr_buf) = match result {
            Ok(tuple) => tuple,
            Err(err_msg) => return Err(err_msg),
        };

        let stdout_str = String::from_utf8_lossy(&stdout_buf);
        let stderr_str = String::from_utf8_lossy(&stderr_buf);

        let mut combined = String::new();
        if !stdout_str.is_empty() {
            combined.push_str(&stdout_str);
        }
        if !stderr_str.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&stderr_str);
        }

        if combined.is_empty() {
            combined = if status.success() {
                "(Command executed successfully with no output)".to_string()
            } else {
                format!("(Command exited with status {:?} and no output)", status.code())
            };
        }

        if status.success() {
            Ok(combined)
        } else {
            Err(format!("Process exit error ({:?}):\n{}", status.code(), combined))
        }
    }
}
