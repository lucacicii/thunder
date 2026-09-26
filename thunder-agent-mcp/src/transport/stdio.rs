use crate::config::McpServerConfig;
use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::transport::McpTransport;
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

pub struct StdioTransport {
    server_name: String,
    stdin_tx: mpsc::Sender<String>,
    pending_requests: Arc<Mutex<HashMap<String, oneshot::Sender<JsonRpcResponse>>>>,
    is_alive: Arc<AtomicBool>,
}

impl StdioTransport {
    /// Spawn the external command and initialize bidirectional JSON-RPC stdio transport.
    pub async fn spawn(
        name: impl Into<String>,
        config: &McpServerConfig,
    ) -> Result<Self, McpError> {
        let server_name = name.into();
        info!(server = %server_name, cmd = %config.command, args = ?config.args, "Spawning MCP stdio server process");

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        if let Some(cwd) = &config.cwd {
            cmd.current_dir(cwd);
        }

        let mut child = cmd.spawn().map_err(|e| {
            McpError::ProcessFailed(format!(
                "Failed to spawn MCP server '{}' ({}): {e}",
                server_name, config.command
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::TransportError("Failed to capture child stdin".to_string()))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::TransportError("Failed to capture child stdout".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            McpError::TransportError("Failed to capture child stderr".to_string())
        })?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(64);
        let pending_requests: Arc<Mutex<HashMap<String, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let is_alive = Arc::new(AtomicBool::new(true));

        let is_alive_stdin = is_alive.clone();
        let name_for_stdin = server_name.clone();

        // 1. Task: Write to stdin
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(msg) = stdin_rx.recv().await {
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    warn!(server = %name_for_stdin, "Failed to write to MCP server stdin: {e}");
                    break;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    warn!(server = %name_for_stdin, "Failed to flush newline to MCP server stdin: {e}");
                    break;
                }
                if let Err(e) = writer.flush().await {
                    warn!(server = %name_for_stdin, "Failed to flush MCP server stdin: {e}");
                    break;
                }
            }
            is_alive_stdin.store(false, Ordering::SeqCst);
        });

        // 2. Task: Read from stdout
        let pending_map = pending_requests.clone();
        let is_alive_stdout = is_alive.clone();
        let name_for_stdout = server_name.clone();

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                debug!(server = %name_for_stdout, raw_line = %trimmed, "MCP stdout received");

                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                    let key = match &resp.id {
                        serde_json::Value::Number(n) => n.to_string(),
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };

                    let mut map = pending_map.lock().await;
                    if let Some(tx) = map.remove(&key) {
                        let _ = tx.send(resp);
                    } else {
                        debug!(server = %name_for_stdout, key = %key, "Received response for unknown request ID");
                    }
                }
            }
            is_alive_stdout.store(false, Ordering::SeqCst);
        });

        // 3. Task: Drain stderr for logs
        let name_for_stderr = server_name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                debug!(server = %name_for_stderr, stderr = %line, "MCP stderr");
            }
        });

        Ok(Self {
            server_name,
            stdin_tx,
            pending_requests,
            is_alive,
        })
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse, McpError> {
        if !self.is_alive() {
            return Err(McpError::NotConnected(format!(
                "MCP server '{}' process is not running",
                self.server_name
            )));
        }

        let id_key = match &request.id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending_requests.lock().await;
            map.insert(id_key.clone(), tx);
        }

        let serialized = serde_json::to_string(&request)
            .map_err(|e| McpError::SerializationError(e.to_string()))?;

        if let Err(e) = self.stdin_tx.send(serialized).await {
            let mut map = self.pending_requests.lock().await;
            map.remove(&id_key);
            return Err(McpError::TransportError(format!(
                "Failed to send request to '{}': {e}",
                self.server_name
            )));
        }

        match rx.await {
            Ok(resp) => Ok(resp),
            Err(_) => Err(McpError::TransportError(format!(
                "MCP response channel dropped for request ID '{id_key}'"
            ))),
        }
    }

    async fn send_notification(&self, notification: JsonRpcNotification) -> Result<(), McpError> {
        if !self.is_alive() {
            return Err(McpError::NotConnected(format!(
                "MCP server '{}' process is not running",
                self.server_name
            )));
        }

        let serialized = serde_json::to_string(&notification)
            .map_err(|e| McpError::SerializationError(e.to_string()))?;

        self.stdin_tx.send(serialized).await.map_err(|e| {
            McpError::TransportError(format!(
                "Failed to send notification to '{}': {e}",
                self.server_name
            ))
        })
    }

    fn is_alive(&self) -> bool {
        self.is_alive.load(Ordering::SeqCst)
    }

    async fn close(&self) -> Result<(), McpError> {
        self.is_alive.store(false, Ordering::SeqCst);
        Ok(())
    }
}
