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
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn bash process: {}", e))?;

        let mut stdout_pipe = child.stdout.take().ok_or("Failed to capture stdout")?;
        let mut stderr_pipe = child.stderr.take().ok_or("Failed to capture stderr")?;

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        let cancel_token = ctx.cancellation_token.clone();

        tokio::select! {
            _ = cancel_token.cancelled() => {
                let _ = child.kill().await;
                Err("Process killed by cancellation signal".to_string())
            }
            res = async {
                let mut out_chunk = [0u8; 4096];
                let mut err_chunk = [0u8; 4096];
                loop {
                    tokio::select! {
                        n = stdout_pipe.read(&mut out_chunk) => {
                            match n {
                                Ok(0) => break,
                                Ok(bytes) => {
                                    if stdout_buf.len() < self.max_buffer_bytes {
                                        stdout_buf.extend_from_slice(&out_chunk[..bytes]);
                                    }
                                }
                                Err(e) => return Err(format!("Stdout read error: {}", e)),
                            }
                        }
                        n = stderr_pipe.read(&mut err_chunk) => {
                            match n {
                                Ok(0) => break,
                                Ok(bytes) => {
                                    if stderr_buf.len() < self.max_buffer_bytes {
                                        stderr_buf.extend_from_slice(&err_chunk[..bytes]);
                                    }
                                }
                                Err(e) => return Err(format!("Stderr read error: {}", e)),
                            }
                        }
                    }
                }
                let status = child.wait().await.map_err(|e| format!("Wait child error: {}", e))?;
                Ok(status)
            } => {
                let status = res?;
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
    }
}
