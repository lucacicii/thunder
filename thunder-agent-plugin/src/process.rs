use crate::protocol::{ClientMessage, HostMessage, PluginMeta, ToolMeta};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thunder_agent_loop::types::config::Permission;
use thunder_agent_loop::types::invoke::{ToolInvocationContext, ToolInvoker};
use thunder_agent_loop::types::ui::{HostUi, NotifyLevel, UiRequest, UiResponse, UiSource};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

#[derive(Clone)]
pub struct SidecarConfig {
    pub runner_path: PathBuf,
    pub plugin_dirs: Vec<PathBuf>,
    pub workspace_dir: PathBuf,
    /// Per-run services, keyed by the call's `route`.
    ///
    /// This replaces what used to be three process-wide values. A single Node
    /// sidecar serves every concurrent run, so a single permission tier / UI /
    /// invoker would let the last-initialised run decide what all the others can
    /// do — including dispatching one run's plugin call into another run's
    /// pipeline, which carries that run's workspace root and path jail.
    pub runs: RunRegistry,
    /// Fallback tier for [`SidecarManager::call_rpc`], the direct test seam that
    /// has no run lifecycle. Real RPCs never read this: they resolve their run's
    /// own tier, or fail closed.
    pub permission: Arc<RwLock<Permission>>,
    /// Fallback UI for the same test seam.
    pub host_ui: Arc<RwLock<Option<Arc<dyn HostUi>>>>,
    /// Fallback invoker for the same test seam.
    pub tool_invoker: Arc<RwLock<Option<Arc<dyn ToolInvoker>>>>,
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
    /// Live view of `SidecarConfig::runs`.
    runs: RunRegistry,
    pending_tool_calls: Arc<RwLock<HashMap<String, PendingRequest>>>,
    pending_prompts: Arc<RwLock<HashMap<String, PendingPromptRequest>>>,
    req_counter: AtomicU64,
}

/// Resolve the services of the run a reverse RPC belongs to.
///
/// Fail-closed on every ambiguity: a call with no `route`, or one whose run has
/// already ended, has no authority to spend. Guessing would mean spending another
/// run's workspace — the pipeline and the jail are per-run.
async fn resolve_run(
    runs: &RunRegistry,
    params: &serde_json::Value,
) -> Result<RunServices, String> {
    let Some(route) = params.get("route").and_then(|v| v.as_str()) else {
        return Err(
            "This call carries no run identifier, so it cannot be authorised. \
             (The plugin host must forward the call's route on every RPC.)"
                .to_string(),
        );
    };
    runs.read().await.get(route).await.ok_or_else(|| {
        format!(
            "Run '{route}' is not active, so its plugin calls are refused. \
             Expected if the run finished while a call was still in flight."
        )
    })
}

/// Dispatch a reverse RPC against one run's already-resolved services.
async fn dispatch_with_run(
    run: RunServices,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let RunServices {
        workspace: ws_dir,
        permission,
        ui: host_ui,
        tools: tool_invoker,
    } = run;

    match method {
        "fs_write_file" => {
            if !permission.allows_write() {
                return Err(
                    "Permission denied: 'fs_write_file' is not granted by the active role"
                        .to_string(),
                );
            }
            let rel_path = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing path")?
                .to_string();
            let content = params
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("Missing content")?
                .to_string();
            let target = ws_dir.join(&rel_path);

            // Path Jail Check
            if !target.starts_with(&ws_dir) {
                return Err(format!(
                    "Security Violation: Path {:?} escapes workspace root",
                    target
                ));
            }

            // Atomic write via .arp/tmp
            let tmp_dir = ws_dir.join(".arp").join("tmp");
            tokio::fs::create_dir_all(&tmp_dir)
                .await
                .map_err(|e| e.to_string())?;
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }

            let tmp_file = tmp_dir.join(format!(
                "tx_{}_{}.tmp",
                std::process::id(),
                fastrand_suffix()
            ));
            tokio::fs::write(&tmp_file, &content)
                .await
                .map_err(|e| e.to_string())?;
            tokio::fs::rename(&tmp_file, &target)
                .await
                .map_err(|e| e.to_string())?;

            Ok(serde_json::json!({
                "success": true,
                "path": target.to_string_lossy(),
                "bytesWritten": content.len()
            }))
        }
        "fs_read_file" => {
            let rel_path = params
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing path")?
                .to_string();
            let target = ws_dir.join(&rel_path);

            if !target.starts_with(&ws_dir) {
                return Err(format!(
                    "Security Violation: Path {:?} escapes workspace root",
                    target
                ));
            }

            let meta = tokio::fs::metadata(&target)
                .await
                .map_err(|e| e.to_string())?;
            if meta.len() > 10 * 1024 * 1024 {
                return Err("File exceeds 10MB memory safety ceiling".to_string());
            }

            let content = tokio::fs::read_to_string(&target)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::Value::String(content))
        }
        "exec_bash" => {
            if !permission.allows_exec() {
                return Err(
                    "Permission denied: 'exec_bash' is not granted by the active role".to_string(),
                );
            }
            let command = params
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or("Missing command")?
                .to_string();
            let cwd = params
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| ws_dir.clone());

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

        // ---- User interaction, always labelled `plugin` on the wire ----
        //
        // The source is hard-coded rather than taken from `params`: a plugin must
        // not be able to claim the reserved `host` channel that panels render
        // with approval chrome.
        "ui_select" | "ui_confirm" | "ui_input" | "ui_editor" => {
            let ui = host_ui
                .ok_or("No user interface available: this host cannot show dialogs".to_string())?;
            let title = params
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(untitled)")
                .to_string();
            let text = |key: &str| params.get(key).and_then(|v| v.as_str()).map(str::to_string);
            let timeout_ms = params.get("timeoutMs").and_then(|v| v.as_u64());

            let request = match method {
                "ui_select" => UiRequest::Select {
                    title,
                    options: params
                        .get("options")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|o| o.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default(),
                    timeout_ms,
                },
                "ui_confirm" => UiRequest::Confirm {
                    title,
                    message: text("message").unwrap_or_default(),
                    timeout_ms,
                },
                "ui_input" => UiRequest::Input {
                    title,
                    placeholder: text("placeholder"),
                    timeout_ms,
                },
                _ => UiRequest::Editor {
                    title,
                    prefill: text("prefill"),
                    timeout_ms,
                },
            };

            let response = ui.request(UiSource::Plugin, request).await;
            Ok(ui_response_json(&response))
        }
        "ui_notify" => {
            let ui = host_ui.ok_or("No user interface available".to_string())?;
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let level = match params.get("level").and_then(|v| v.as_str()) {
                Some("warning") => NotifyLevel::Warning,
                Some("error") => NotifyLevel::Error,
                _ => NotifyLevel::Info,
            };
            ui.notify(UiSource::Plugin, message, level);
            Ok(serde_json::json!({ "ok": true }))
        }
        "ui_set_status" => {
            let ui = host_ui.ok_or("No user interface available".to_string())?;
            let key = params
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or("Missing key")?;
            ui.set_status(
                key,
                params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            );
            Ok(serde_json::json!({ "ok": true }))
        }

        // ---- Calling another tool on the plugin's own initiative ----
        //
        // Routed through the run's own invoker, which dispatches via the full
        // onion. The plugin gets no shortcut around the tier, the jail or the
        // approval gate.
        "call_tool" => {
            let invoker = tool_invoker.ok_or(
                "This run exposes no agent tool pipeline, so plugins cannot call tools".to_string(),
            )?;
            let tool = params
                .get("tool")
                .and_then(|v| v.as_str())
                .ok_or("Missing tool name")?
                .to_string();
            let args = params
                .get("args")
                .cloned()
                .unwrap_or(serde_json::Value::Object(Default::default()));
            let ctx = ToolInvocationContext {
                // The Node side owns the plugin registry, so it is the only place
                // that knows who is asking. An unattributed plugin call would be
                // indistinguishable from one the model made.
                plugin_id: params
                    .get("pluginId")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                session_id: Some(route_of(&params)),
                turn: None,
            };
            let out = invoker.invoke(&tool, args, &ctx).await?;
            Ok(serde_json::Value::String(out))
        }
        _ => Err(format!("Unsupported RPC method: {method}")),
    }
}

/// Echo a call's run identifier back, for error messages and session tagging.
fn route_of(params: &serde_json::Value) -> String {
    params
        .get("route")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

impl SidecarManager {
    pub fn new(config: SidecarConfig) -> Arc<Self> {
        Arc::new(Self {
            runs: Arc::clone(&config.runs),
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
        let node_bin = if Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "node"
        } else if Command::new("bun")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
        {
            "bun"
        } else {
            return Err(
                "Neither 'node' nor 'bun' found in PATH. TypeScript plugins disabled.".to_string(),
            );
        };

        if !self.config.runner_path.exists() {
            return Err(format!(
                "Plugin runner script not found at {:?}",
                self.config.runner_path
            ));
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

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn Node sidecar: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to capture stdin of sidecar".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Failed to capture stdout of sidecar".to_string())?;

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
        let runs = Arc::clone(&self.runs);

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
                        info!(
                            plugins_count = plugins.len(),
                            tools_count = tools.len(),
                            "TypeScript plugin manifest synchronized"
                        );
                        *this.active_plugins.write().await = plugins;
                        *this.active_tools.write().await = tools;
                    }
                    ClientMessage::ToolResult {
                        call_id,
                        success,
                        output,
                        error,
                    } => {
                        let mut map = pending_calls.write().await;
                        if let Some(req) = map.remove(&call_id) {
                            if success {
                                let _ = req.tx.send(Ok(output.unwrap_or_default()));
                            } else {
                                let _ =
                                    req.tx
                                        .send(Err(error
                                            .unwrap_or_else(|| "Unknown tool error".to_string())));
                            }
                        }
                    }
                    ClientMessage::SystemPromptsResult {
                        request_id,
                        prompts,
                    } => {
                        let mut map = pending_prompts.write().await;
                        if let Some(req) = map.remove(&request_id) {
                            let _ = req.tx.send(prompts);
                        }
                    }
                    ClientMessage::RpcRequest { id, method, params } => {
                        let resp = Self::handle_client_rpc(&runs, &method, params).await;
                        this.send_message(HostMessage::RpcResponse {
                            id,
                            success: resp.is_ok(),
                            data: resp.as_ref().ok().cloned(),
                            error: resp.err(),
                        })
                        .await;
                    }
                    ClientMessage::ReloadAck {
                        success,
                        plugin_id,
                        error,
                        kept_active,
                    } => {
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
        })
        .await;

        Ok(())
    }

    pub async fn send_message(&self, msg: HostMessage) {
        if let Some(tx) = self.stdin_tx.read().await.as_ref() {
            let _ = tx.send(msg).await;
        }
    }

    /// Resolve the caller's run, then dispatch.
    async fn handle_client_rpc(
        runs: &RunRegistry,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let run = resolve_run(runs, &params).await?;
        dispatch_with_run(run, method, params).await
    }

    /// Invoke a reverse RPC (the channel behind `ctx.fs` / `ctx.exec`) directly.
    ///
    /// Uses this sidecar's configured workspace and permission tier, so callers
    /// and tests observe exactly the same enforcement as an in-process plugin.
    pub async fn call_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        // Test/embedder seam: there is no run lifecycle here, so the sidecar's
        // configured defaults stand in for a run's services. Real RPCs never take
        // this path — they resolve their own run, or fail closed.
        let run = RunServices {
            workspace: self.config.workspace_dir.clone(),
            permission: *self.config.permission.read().await,
            ui: self.config.host_ui.read().await.clone(),
            tools: self.config.tool_invoker.read().await.clone(),
        };
        dispatch_with_run(run, method, params).await
    }

    /// Replace the capability tier used by [`SidecarManager::call_rpc`], the
    /// test seam that has no run lifecycle. Real RPCs resolve their own run.
    pub async fn set_permission(&self, permission: Permission) {
        *self.config.permission.write().await = permission;
    }

    /// The per-run services registry.
    pub fn runs(&self) -> RunRegistry {
        Arc::clone(&self.runs)
    }

    pub async fn execute_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        context: serde_json::Value,
    ) -> Result<String, String> {
        let call_id = format!(
            "call_{}_{}",
            self.req_counter.fetch_add(1, Ordering::SeqCst),
            fastrand_suffix()
        );
        let (tx, rx) = oneshot::channel();

        self.pending_tool_calls
            .write()
            .await
            .insert(call_id.clone(), PendingRequest { tx });

        self.send_message(HostMessage::ExecuteTool {
            call_id: call_id.clone(),
            tool_name: tool_name.to_string(),
            args,
            context,
        })
        .await;

        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("Tool response channel closed prematurely".to_string()),
            Err(_) => {
                self.pending_tool_calls.write().await.remove(&call_id);
                Err(format!(
                    "Execution of tool '{tool_name}' timed out after 60s"
                ))
            }
        }
    }

    pub async fn get_system_prompts(
        &self,
        context: serde_json::Value,
    ) -> Vec<crate::protocol::PromptContribution> {
        let request_id = format!(
            "prompt_{}_{}",
            self.req_counter.fetch_add(1, Ordering::SeqCst),
            fastrand_suffix()
        );
        let (tx, rx) = oneshot::channel();

        self.pending_prompts
            .write()
            .await
            .insert(request_id.clone(), PendingPromptRequest { tx });

        self.send_message(HostMessage::GetSystemPrompts {
            request_id: request_id.clone(),
            context,
        })
        .await;

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(prompts)) => prompts,
            _ => {
                self.pending_prompts.write().await.remove(&request_id);
                Vec::new()
            }
        }
    }

    pub async fn dispatch_event(&self, event: serde_json::Value, context: serde_json::Value) {
        self.send_message(HostMessage::DispatchEvent { event, context })
            .await;
    }

    pub async fn reload(&self, path: Option<PathBuf>) {
        self.send_message(HostMessage::Reload {
            path,
            plugin_dirs: Some(self.config.plugin_dirs.clone()),
        })
        .await;
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

/// Allocate the shared capability slot a [`SidecarConfig`] carries.
pub fn permission_slot(permission: Permission) -> Arc<RwLock<Permission>> {
    Arc::new(RwLock::new(permission))
}

/// Everything a run lends to the shared sidecar.
///
/// The sidecar is one Node process serving every concurrent run, so nothing it
/// can reach may be stored as a single process-wide value: whichever run
/// initialised last would decide the authority available to all the others.
/// The pipeline in particular carries the workspace root and the path jail, so
/// a misrouted call is a cross-workspace write, not merely a permission slip.
#[derive(Clone, Default)]
pub struct RunServices {
    /// Workspace root this run is jailed to.
    ///
    /// Carried here rather than read from the sidecar config because a run's
    /// workspace is not the sidecar's: the daemon boots the sidecar with its
    /// default workspace, and a task may target another directory. Without this,
    /// a plugin's writes would be jailed to the wrong tree.
    pub workspace: PathBuf,
    /// Capability tier this run's plugins inherit.
    pub permission: Permission,
    /// Panel for `ctx.ui`. `None` = this run has no panel, so every dialog is
    /// cancelled.
    pub ui: Option<Arc<dyn HostUi>>,
    /// Pipeline for `ctx.callTool`. `None` until the run's tools are all
    /// registered, so early calls are refused rather than misrouted.
    pub tools: Option<Arc<dyn ToolInvoker>>,
}

/// Per-run services, keyed by [`ToolExecutionContext::route`][route].
///
/// [route]: thunder_agent_loop::types::tool::ToolExecutionContext::route
///
/// Bounded: a run that never reaches `on_finish` (a crashed or detached host)
/// would otherwise leak its entry forever, so the oldest is evicted once the
/// cap is hit. Eviction degrades to a refusal, never to a wider grant.
pub type RunRegistry = Arc<RwLock<RunRegistryInner>>;

#[derive(Default)]
pub struct RunRegistryInner {
    by_route: HashMap<String, RunServices>,
    /// Insertion order, for FIFO eviction.
    order: VecDeque<String>,
    limit: usize,
}

/// Concurrent runs the registry will track before evicting.
pub const DEFAULT_RUN_REGISTRY_LIMIT: usize = 64;

impl RunRegistryInner {
    fn evict_if_needed(&mut self, incoming: &str) {
        // A re-registration must not consume capacity or reorder the queue.
        if self.by_route.contains_key(incoming) {
            return;
        }
        if self.limit == 0 {
            self.limit = DEFAULT_RUN_REGISTRY_LIMIT;
        }
        while self.by_route.len() >= self.limit {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.by_route.remove(&oldest);
            warn!(evicted = %oldest, "Run registry full; evicted the oldest run's services");
        }
        self.order.push_back(incoming.to_string());
    }
}

pub fn run_registry() -> RunRegistry {
    Arc::new(RwLock::new(RunRegistryInner {
        by_route: HashMap::new(),
        order: VecDeque::new(),
        limit: DEFAULT_RUN_REGISTRY_LIMIT,
    }))
}

impl RunRegistryInner {
    /// Register (or refresh) a run's tier and UI.
    ///
    /// `tools` is left alone when absent so the second phase — filling the
    /// invoker once the pipeline is final — does not have to restate the rest.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_run(
        &mut self,
        route: &str,
        workspace: PathBuf,
        permission: Permission,
        ui: Option<Arc<dyn HostUi>>,
    ) {
        self.evict_if_needed(route);
        self.by_route
            .entry(route.to_string())
            .and_modify(|e| {
                e.workspace = workspace.clone();
                e.permission = permission;
                e.ui = ui.clone();
            })
            .or_insert_with(|| RunServices {
                workspace,
                permission,
                ui,
                tools: None,
            });
    }

    pub async fn set_tools(&mut self, route: &str, tools: Arc<dyn ToolInvoker>) {
        if let Some(entry) = self.by_route.get_mut(route) {
            entry.tools = Some(tools);
        }
    }

    pub async fn end_run(&mut self, route: &str) {
        self.by_route.remove(route);
        self.order.retain(|r| r != route);
    }

    pub async fn get(&self, route: &str) -> Option<RunServices> {
        self.by_route.get(route).cloned()
    }

    /// How many runs are currently tracked. Synchronous: it reads a plain `len`.
    pub fn len(&self) -> usize {
        self.by_route.len()
    }

    /// Whether no run is registered.
    pub fn is_empty(&self) -> bool {
        self.by_route.is_empty()
    }
}

/// `[`UiResponse`]` as the JSON the Node side expects.
fn ui_response_json(response: &UiResponse) -> serde_json::Value {
    serde_json::to_value(response).unwrap_or(serde_json::Value::Null)
}
