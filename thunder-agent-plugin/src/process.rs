use crate::protocol::{ClientMessage, HostMessage, PluginMeta, ToolMeta};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

pub struct SidecarConfig {
    pub runner_path: PathBuf,
    pub plugin_dirs: Vec<PathBuf>,
    pub workspace_dir: PathBuf,
}

struct PendingRequest {
    tx: oneshot::Sender<Result<String, String>>,
}

struct PendingPromptRequest {
    tx: oneshot::Sender<Vec<crate::protocol::PromptContribution>>,
}

pub struct SidecarManager {
    config: SidecarConfig,
    stdin_tx: RwLock<Option<mpsc::Sender<HostMessage>>>,
    active_plugins: RwLock<Vec<PluginMeta>>,
    active_tools: RwLock<Vec<ToolMeta>>,
    pending_tool_calls: Arc<RwLock<HashMap<String, PendingRequest>>>,
    pending_prompts: Arc<RwLock<HashMap<String, PendingPromptRequest>>>,
    req_counter: AtomicU64,
}

impl SidecarManager {
    pub fn new(config: SidecarConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            stdin_tx: RwLock::new(None),
            active_plugins: RwLock::new(Vec::new()),
            active_tools: RwLock::new(Vec::new()),
            pending_tool_calls: Arc::new(RwLock::new(HashMap::new())),
            pending_prompts: Arc::new(RwLock::new(HashMap::new())),
            req_counter: AtomicU64::new(1),
        })
    }

    /// Check if Node.js runtime is installed on the host machine.
    pub async fn is_node_available() -> bool {
        Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Start or restart the Node.js sidecar process.
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let node_bin = if Command::new("node").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().await.map(|s| s.success()).unwrap_or(false) {
            "node"
        } else if Command::new("bun").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().await.map(|s| s.success()).unwrap_or(false) {
            "bun"
        } else {
            return Err("Neither 'node' nor 'bun' found in PATH. TypeScript plugins disabled.".to_string());
        };

        if !self.config.runner_path.exists() {
            return Err(format!("Plugin runner script not found at {:?}", self.config.runner_path));
        }

        info!(runner = ?self.config.runner_path, "Starting TypeScript Plugin Sidecar runner");

        let mut cmd = Command::new(node_bin);
        cmd.arg(&self.config.runner_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .current_dir(&self.config.workspace_dir);

        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn Node sidecar: {e}"))?;
        let stdin = child.stdin.take().ok_or_else(|| "Failed to capture stdin of sidecar".to_string())?;
        let stdout = child.stdout.take().ok_or_else(|| "Failed to capture stdout of sidecar".to_string())?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<HostMessage>(64);
        *self.stdin_tx.write().await = Some(stdin_tx);

        // STDIN writer task
        tokio::spawn(async move {
            let mut writer = stdin;
            while let Some(msg) = stdin_rx.recv().await {
                if let Ok(json_line) = serde_json::to_string(&msg) {
                    if writer.write_all(json_line.as_bytes()).await.is_err()
                        || writer.write_all(b"\n").await.is_err()
                        || writer.flush().await.is_err()
                    {
                        break;
                    }
                }
            }
        });

        // STDOUT reader task
        let this = Arc::clone(self);
        let pending_calls = Arc::clone(&self.pending_tool_calls);
        let pending_prompts = Arc::clone(&self.pending_prompts);
        let ws_dir = self.config.workspace_dir.clone();

        tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let msg: ClientMessage = match serde_json::from_str(trimmed) {
                    Ok(m) => m,
                    Err(err) => {
                        debug!(raw = %trimmed, error = %err, "Ignoring non-protocol output from sidecar");
                        continue;
                    }
                };

                match msg {
                    ClientMessage::ManifestSynced { plugins, tools } => {
                        info!(plugins_count = plugins.len(), tools_count = tools.len(), "TypeScript plugin manifest synchronized");
                        *this.active_plugins.write().await = plugins;
                        *this.active_tools.write().await = tools;
                    }
                    ClientMessage::ToolResult { call_id, success, output, error } => {
                        let mut map = pending_calls.write().await;
                        if let Some(req) = map.remove(&call_id) {
                            if success {
                                let _ = req.tx.send(Ok(output.unwrap_or_default()));
                            } else {
                                let _ = req.tx.send(Err(error.unwrap_or_else(|| "Unknown tool error".to_string())));
                            }
                        }
                    }
                    ClientMessage::SystemPromptsResult { request_id, prompts } => {
                        let mut map = pending_prompts.write().await;
                        if let Some(req) = map.remove(&request_id) {
                            let _ = req.tx.send(prompts);
                        }
                    }
                    ClientMessage::RpcRequest { id, method, params } => {
                        let resp = Self::handle_client_rpc(&ws_dir, &method, params).await;
                        this.send_message(HostMessage::RpcResponse {
                            id,
                            success: resp.is_ok(),
                            data: resp.as_ref().ok().cloned(),
                            error: resp.err(),
                        }).await;
                    }
                    ClientMessage::ReloadAck { success, plugin_id, error, kept_active } => {
                        if success {
                            info!(plugin_id = ?plugin_id, "TypeScript plugin reloaded successfully");
                        } else {
                            warn!(plugin_id = ?plugin_id, error = ?error, kept_active = ?kept_active, "TypeScript plugin reload error handled gracefully");
                        }
                    }
                    ClientMessage::InitAck { .. } => {}
                }
            }

            warn!("Node.js plugin sidecar STDOUT closed. Waiting for child process exit...");
            let _ = child.wait().await;
            *this.stdin_tx.write().await = None;
        });

        // Initialize sidecar with configured directories
        self.send_message(HostMessage::Init {
            plugin_dirs: self.config.plugin_dirs.clone(),
        }).await;

        Ok(())
    }

    pub async fn send_message(&self, msg: HostMessage) {
        if let Some(tx) = self.stdin_tx.read().await.as_ref() {
            let _ = tx.send(msg).await;
        }
    }

    /// Handle reverse RPC requested by TS plugin (`ctx.fs.writeFile`, `ctx.exec`, etc.)
    async fn handle_client_rpc(ws_dir: &Path, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        match method {
            "fs_write_file" => {
                let rel_path = params.get("path").and_then(|v| v.as_str()).ok_or("Missing path")?.to_string();
                let content = params.get("content").and_then(|v| v.as_str()).ok_or("Missing content")?.to_string();
                let target = ws_dir.join(&rel_path);

                // Path Jail Check
                if !target.starts_with(ws_dir) {
                    return Err(format!("Security Violation: Path {:?} escapes workspace root", target));
                }

                // Atomic write via .arp/tmp
                let tmp_dir = ws_dir.join(".arp").join("tmp");
                tokio::fs::create_dir_all(&tmp_dir).await.map_err(|e| e.to_string())?;
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
                }

                let tmp_file = tmp_dir.join(format!("tx_{}_{}.tmp", std::process::id(), fastrand_suffix()));
                tokio::fs::write(&tmp_file, &content).await.map_err(|e| e.to_string())?;
                tokio::fs::rename(&tmp_file, &target).await.map_err(|e| e.to_string())?;

                Ok(serde_json::json!({
                    "success": true,
                    "path": target.to_string_lossy(),
                    "bytesWritten": content.len()
                }))
            }
            "fs_read_file" => {
                let rel_path = params.get("path").and_then(|v| v.as_str()).ok_or("Missing path")?.to_string();
                let target = ws_dir.join(&rel_path);

                if !target.starts_with(ws_dir) {
                    return Err(format!("Security Violation: Path {:?} escapes workspace root", target));
                }

                let meta = tokio::fs::metadata(&target).await.map_err(|e| e.to_string())?;
                if meta.len() > 10 * 1024 * 1024 {
                    return Err("File exceeds 10MB memory safety ceiling".to_string());
                }

                let content = tokio::fs::read_to_string(&target).await.map_err(|e| e.to_string())?;
                Ok(serde_json::Value::String(content))
            }
            "exec_bash" => {
                let command = params.get("command").and_then(|v| v.as_str()).ok_or("Missing command")?.to_string();
                let cwd = params.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from).unwrap_or_else(|| ws_dir.to_path_buf());

                let mut cmd = Command::new("bash");
                cmd.arg("-c").arg(&command).current_dir(&cwd);

                #[cfg(unix)]
                {
                    cmd.process_group(0);
                }

                let output = cmd.output().await.map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "exitCode": output.status.code().unwrap_or(-1),
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr)
                }))
            }
            _ => Err(format!("Unsupported RPC method: {method}")),
        }
    }

    pub async fn execute_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        context: serde_json::Value,
    ) -> Result<String, String> {
        let call_id = format!("call_{}_{}", self.req_counter.fetch_add(1, Ordering::SeqCst), fastrand_suffix());
        let (tx, rx) = oneshot::channel();

        self.pending_tool_calls.write().await.insert(call_id.clone(), PendingRequest { tx });

        self.send_message(HostMessage::ExecuteTool {
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            args,
            context,
        }).await;

        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("Tool response channel closed prematurely".to_string()),
            Err(_) => {
                self.pending_tool_calls.write().await.remove(&call_id);
                Err(format!("Execution of tool '{tool_name}' timed out after 60s"))
            }
        }
    }

    pub async fn get_system_prompts(&self, context: serde_json::Value) -> Vec<crate::protocol::PromptContribution> {
        let request_id = format!("prompt_{}_{}", self.req_counter.fetch_add(1, Ordering::SeqCst), fastrand_suffix());
        let (tx, rx) = oneshot::channel();

        self.pending_prompts.write().await.insert(request_id.clone(), PendingPromptRequest { tx });

        self.send_message(HostMessage::GetSystemPrompts {
            request_id: request_id.clone(),
            context,
        }).await;

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(prompts)) => prompts,
            _ => {
                self.pending_prompts.write().await.remove(&request_id);
                Vec::new()
            }
        }
    }

    pub async fn dispatch_event(&self, event: serde_json::Value, context: serde_json::Value) {
        self.send_message(HostMessage::DispatchEvent { event, context }).await;
    }

    pub async fn reload(&self, path: Option<PathBuf>) {
        self.send_message(HostMessage::Reload {
            path,
            plugin_dirs: Some(self.config.plugin_dirs.clone()),
        }).await;
    }

    pub async fn list_tools(&self) -> Vec<ToolMeta> {
        self.active_tools.read().await.clone()
    }

    pub async fn list_plugins(&self) -> Vec<PluginMeta> {
        self.active_plugins.read().await.clone()
    }
}



fn fastrand_suffix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
