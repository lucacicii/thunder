use crate::ansi::strip_ansi_codes;
use crate::flood_guard::{drain_stream_with_flood_guard, DEFAULT_MAX_OUTPUT_BYTES};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use thunder_protocol::{ExecutionContext, ToolDescriptor, ToolExecutionError, ToolProvider};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Execute a TSP v1 isolated script in its own process group with flood protection (§3.2, §5.1, §5.2, §6.3).
pub async fn execute_isolated_script(
    script_path: &Path,
    arguments: Value,
    ctx: &ExecutionContext,
) -> Result<String, ToolExecutionError> {
    execute_isolated_script_with_limit(script_path, arguments, ctx, DEFAULT_MAX_OUTPUT_BYTES).await
}

/// Execute an isolated script with a configurable output byte limit.
pub async fn execute_isolated_script_with_limit(
    script_path: &Path,
    arguments: Value,
    ctx: &ExecutionContext,
    max_output_bytes: usize,
) -> Result<String, ToolExecutionError> {
    let mut cmd = Command::new(script_path);
    cmd.current_dir(&ctx.workspace_cwd)
        .env("THUNDER_CWD", &ctx.workspace_cwd)
        .env("THUNDER_SESSION_ID", &ctx.session_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    run_sandboxed_command(cmd, Some(arguments), ctx, max_output_bytes).await
}

/// Execute a shell command (`sh -c <command>`) in an isolated process group with flood protection.
pub async fn execute_shell_command(
    command: &str,
    ctx: &ExecutionContext,
    max_output_bytes: usize,
) -> Result<String, ToolExecutionError> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut cmd = Command::new(shell);
    cmd.arg("-c")
        .arg(command)
        .current_dir(&ctx.workspace_cwd)
        .env("THUNDER_CWD", &ctx.workspace_cwd)
        .env("THUNDER_SESSION_ID", &ctx.session_id)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("CI", "true")
        .env("TERM", "dumb")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    cmd.process_group(0);

    run_sandboxed_command(cmd, None, ctx, max_output_bytes).await
}

async fn run_sandboxed_command(
    mut cmd: Command,
    stdin_json: Option<Value>,
    ctx: &ExecutionContext,
    max_output_bytes: usize,
) -> Result<String, ToolExecutionError> {
    let mut child = cmd.spawn().map_err(ToolExecutionError::IOError)?;

    // Deliver JSON arguments via stdin and close EOF (§3.2).
    if let Some(arguments) = stdin_json {
        if let Some(mut stdin) = child.stdin.take() {
            let payload = serde_json::to_vec(&arguments)
                .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;
            tokio::spawn(async move {
                let _ = stdin.write_all(&payload).await;
                let _ = stdin.shutdown().await;
            });
        }
    }

    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| ToolExecutionError::IOError(std::io::Error::other("missing stdout pipe")))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| ToolExecutionError::IOError(std::io::Error::other("missing stderr pipe")))?;

    // Drain stdout and stderr concurrently in independent tasks so the child process can never
    // block on a full OS pipe buffer regardless of how much it writes to either stream.
    let session_out = ctx.session_id.clone();
    let stdout_task = tokio::spawn(async move {
        drain_stream_with_flood_guard(stdout_pipe, max_output_bytes, &session_out, "stdout").await
    });

    let session_err = ctx.session_id.clone();
    let stderr_task = tokio::spawn(async move {
        drain_stream_with_flood_guard(stderr_pipe, max_output_bytes, &session_err, "stderr").await
    });

    let wait_outcome = tokio::time::timeout(ctx.timeout, child.wait()).await;

    match wait_outcome {
        Ok(Ok(status)) => {
            let stdout_res = stdout_task
                .await
                .map_err(|e| ToolExecutionError::IOError(std::io::Error::other(e.to_string())))?
                .map_err(ToolExecutionError::IOError)?;
            let stderr_res = stderr_task
                .await
                .map_err(|e| ToolExecutionError::IOError(std::io::Error::other(e.to_string())))?
                .map_err(ToolExecutionError::IOError)?;

            if status.success() {
                Ok(stdout_res.text)
            } else {
                let raw_err = if !stderr_res.text.trim().is_empty() {
                    stderr_res.text
                } else {
                    stdout_res.text
                };
                Err(ToolExecutionError::NonZeroExit {
                    code: status.code().unwrap_or(-1),
                    stderr: strip_ansi_codes(&raw_err),
                })
            }
        }
        Ok(Err(e)) => {
            stdout_task.abort();
            stderr_task.abort();
            Err(ToolExecutionError::IOError(e))
        }
        Err(_) => {
            // Timeout triggered: kill the entire process group with SIGKILL and reap the child
            // so no orphan descendants or zombie processes remain (§5.1).
            kill_process_group(&child);
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            Err(ToolExecutionError::Timeout)
        }
    }
}

#[cfg(unix)]
fn kill_process_group(child: &tokio::process::Child) {
    if let Some(pid) = child.id() {
        if pid > 0 {
            unsafe {
                // Negative PID sends SIGKILL to every process in the child's process group.
                let _ = libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(_child: &tokio::process::Child) {}

/// Entry for a discovered TSP v1 local script tool.
#[derive(Clone, Debug)]
pub struct ScriptEntry {
    pub descriptor: ToolDescriptor,
    pub script_path: PathBuf,
}

/// [`ToolProvider`] implementation backed by TSP v1 local scripts in `.thunder/tools/`.
pub struct ScriptToolProvider {
    provider_id: String,
    scripts: RwLock<HashMap<String, ScriptEntry>>,
    max_output_bytes: usize,
}

impl ScriptToolProvider {
    pub fn new(provider_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            scripts: RwLock::new(HashMap::new()),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    pub fn upsert_script(&self, entry: ScriptEntry) {
        let mut guard = self.scripts.write();
        guard.insert(entry.descriptor.name.clone(), entry.clone());
        guard.insert(entry.descriptor.logical_name(), entry);
    }

    pub fn replace_all(&self, entries: Vec<ScriptEntry>) {
        let mut map = HashMap::new();
        for entry in entries {
            map.insert(entry.descriptor.name.clone(), entry.clone());
            map.insert(entry.descriptor.logical_name(), entry);
        }
        *self.scripts.write() = map;
    }

    pub fn shared(provider_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self::new(provider_id))
    }
}

#[async_trait]
impl ToolProvider for ScriptToolProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        let guard = self.scripts.read();
        let mut seen = std::collections::BTreeMap::new();
        for entry in guard.values() {
            seen.insert(entry.descriptor.name.clone(), entry.descriptor.clone());
        }
        Ok(seen.into_values().collect())
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        let script_path = {
            let guard = self.scripts.read();
            guard
                .get(name)
                .or_else(|| {
                    let stripped = name.strip_prefix("script__")?;
                    guard.get(stripped)
                })
                .map(|e| e.script_path.clone())
        }
        .ok_or_else(|| ToolExecutionError::ToolNotFound(name.to_string()))?;

        execute_isolated_script_with_limit(&script_path, arguments, ctx, self.max_output_bytes)
            .await
    }
}
