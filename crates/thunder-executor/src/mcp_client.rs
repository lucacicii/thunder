use crate::ansi::strip_ansi_codes;
use crate::flood_guard::{drain_stream_with_flood_guard, DEFAULT_MAX_OUTPUT_BYTES};
use async_trait::async_trait;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thunder_protocol::{
    ContextEnvelopeMeta, ExecutionContext, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    McpCallToolResult, McpListToolsResult, McpToolCallParams, ToolDescriptor, ToolExecutionError,
    ToolProvider,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

/// Maximum size of a single inbound JSON-RPC frame (4 MiB).
pub const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Declarative configuration for a persistent MCP Stdio server process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Whether this MCP server provides the core standard library (`core://...`) rather than an external `mcp://<server>/...`.
    #[serde(default)]
    pub is_core: bool,
}

impl McpServerConfig {
    pub fn new(command: impl AsRef<std::ffi::OsStr>) -> Self {
        Self {
            command: command.as_ref().to_string_lossy().into_owned(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            is_core: false,
        }
    }

    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn as_core(mut self) -> Self {
        self.is_core = true;
        self
    }
}

#[derive(Clone)]
struct PendingMap(Arc<Mutex<HashMap<String, oneshot::Sender<JsonRpcResponse>>>>);

impl PendingMap {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())))
    }

    fn insert(&self, key: String, tx: oneshot::Sender<JsonRpcResponse>) {
        self.0.lock().insert(key, tx);
    }

    fn remove(&self, key: &str) -> Option<oneshot::Sender<JsonRpcResponse>> {
        self.0.lock().remove(key)
    }

    fn clear(&self) {
        self.0.lock().clear();
    }
}

struct PendingEntry {
    map: PendingMap,
    key: String,
}

impl PendingEntry {
    fn register(map: &PendingMap, key: String, tx: oneshot::Sender<JsonRpcResponse>) -> Self {
        map.insert(key.clone(), tx);
        Self {
            map: map.clone(),
            key,
        }
    }
}

impl Drop for PendingEntry {
    fn drop(&mut self) {
        self.map.remove(&self.key);
    }
}

/// Persistent JSON-RPC 2.0 client over child process `stdin`/`stdout`.
pub struct StdioMcpClient {
    server_name: String,
    stdin_tx: mpsc::Sender<String>,
    pending: PendingMap,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    _child: Arc<tokio::sync::Mutex<Option<Child>>>,
}

impl StdioMcpClient {
    /// Spawn the child MCP server process and perform the MCP `initialize` handshake.
    pub async fn connect(
        server_name: impl Into<String>,
        config: &McpServerConfig,
    ) -> Result<Arc<Self>, ToolExecutionError> {
        let server_name = server_name.into();
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .envs(&config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        if let Some(ref cwd) = config.cwd {
            cmd.current_dir(cwd);
        }

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn().map_err(ToolExecutionError::IOError)?;
        let mut child_stdin = child.stdin.take().ok_or_else(|| {
            ToolExecutionError::IOError(std::io::Error::other("missing MCP stdin"))
        })?;
        let child_stdout = child.stdout.take().ok_or_else(|| {
            ToolExecutionError::IOError(std::io::Error::other("missing MCP stdout"))
        })?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(128);
        let pending = PendingMap::new();
        let alive = Arc::new(AtomicBool::new(true));

        let alive_writer = Arc::clone(&alive);
        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                if child_stdin.write_all(line.as_bytes()).await.is_err()
                    || child_stdin.write_all(b"\n").await.is_err()
                    || child_stdin.flush().await.is_err()
                {
                    alive_writer.store(false, Ordering::SeqCst);
                    break;
                }
            }
        });

        let pending_reader = pending.clone();
        let alive_reader = Arc::clone(&alive);
        let srv_tag = server_name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(child_stdout);
            loop {
                match read_bounded_line(&mut reader, DEFAULT_MAX_FRAME_BYTES).await {
                    Ok(Some(Some(line))) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(trimmed) {
                            let id_key = id_to_key(&resp.id);
                            if let Some(tx) = pending_reader.remove(&id_key) {
                                let _ = tx.send(resp);
                            }
                        } else {
                            debug!(server = %srv_tag, "Ignoring non-response JSON-RPC frame from MCP server");
                        }
                    }
                    Ok(Some(None)) => {
                        warn!(server = %srv_tag, "MCP server frame exceeded max_frame_bytes; dropping pending requests");
                        pending_reader.clear();
                    }
                    Ok(None) | Err(_) => {
                        alive_reader.store(false, Ordering::SeqCst);
                        pending_reader.clear();
                        break;
                    }
                }
            }
        });

        let client = Arc::new(Self {
            server_name,
            stdin_tx,
            pending,
            next_id: AtomicU64::new(1),
            alive,
            _child: Arc::new(tokio::sync::Mutex::new(Some(child))),
        });

        // Perform standard MCP initialize handshake
        let _ = client
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "thunder-core",
                        "version": "0.1.0"
                    }
                })),
                Duration::from_secs(15),
            )
            .await?;

        let _ = client
            .notify("notifications/initialized", Some(json!({})))
            .await;

        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub async fn notify(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), ToolExecutionError> {
        if !self.is_alive() {
            return Err(ToolExecutionError::ProtocolError(format!(
                "MCP server '{}' is not alive",
                self.server_name
            )));
        }
        let notif = JsonRpcNotification::new(method, params);
        let serialized = serde_json::to_string(&notif)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;
        self.stdin_tx.send(serialized).await.map_err(|_| {
            ToolExecutionError::ProtocolError(format!(
                "MCP server '{}' stdin channel closed",
                self.server_name
            ))
        })
    }

    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, ToolExecutionError> {
        if !self.is_alive() {
            return Err(ToolExecutionError::ProtocolError(format!(
                "MCP server '{}' process disconnected",
                self.server_name
            )));
        }

        let seq = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req_id = format!("req-{seq}");
        let req = JsonRpcRequest::new(Value::String(req_id.clone()), method, params);
        let serialized = serde_json::to_string(&req)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;

        let (tx, rx) = oneshot::channel();
        let _guard = PendingEntry::register(&self.pending, req_id, tx);

        self.stdin_tx.send(serialized).await.map_err(|_| {
            ToolExecutionError::ProtocolError(format!(
                "Failed to write request to MCP server '{}'",
                self.server_name
            ))
        })?;

        let response = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| ToolExecutionError::Timeout)?
            .map_err(|_| {
                ToolExecutionError::ProtocolError(format!(
                    "MCP server '{}' closed pipe before replying",
                    self.server_name
                ))
            })?;

        if let Some(err) = response.error {
            return Err(ToolExecutionError::NonZeroExit {
                code: err.code,
                stderr: strip_ansi_codes(&err.message),
            });
        }

        Ok(response.result.unwrap_or(Value::Null))
    }
}

fn id_to_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Read a single newline-delimited JSON-RPC frame up to `max_bytes`.
/// Returns `Ok(Some(Some(line)))` for a normal line, `Ok(Some(None))` when oversized, or `Ok(None)` on EOF.
async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<Option<String>>> {
    let mut buf = Vec::new();
    let mut oversized = false;
    let mut total = 0usize;

    loop {
        let (consumed, found_newline) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                if total == 0 && !oversized {
                    return Ok(None);
                }
                break;
            }
            let newline_pos = available.iter().position(|b| *b == b'\n');
            let take = newline_pos.unwrap_or(available.len());
            total += take;

            if !oversized {
                if buf.len() + take > max_bytes {
                    oversized = true;
                } else {
                    buf.extend_from_slice(&available[..take]);
                }
            }
            (
                take + usize::from(newline_pos.is_some()),
                newline_pos.is_some(),
            )
        };
        reader.consume(consumed);
        if found_newline {
            break;
        }
    }

    if oversized {
        Ok(Some(None))
    } else {
        Ok(Some(Some(String::from_utf8_lossy(&buf).into_owned())))
    }
}

/// [`ToolProvider`] wrapping a persistent MCP server (`thunder-std` when `is_core = true`, or external MCP).
pub struct McpToolProvider {
    provider_id: String,
    server_name: String,
    is_core: bool,
    client: Arc<StdioMcpClient>,
    cached_schemas: RwLock<HashMap<String, Value>>,
    max_output_bytes: usize,
}

impl McpToolProvider {
    pub async fn connect(
        server_name: impl Into<String>,
        config: &McpServerConfig,
    ) -> Result<Arc<Self>, ToolExecutionError> {
        let s_name = server_name.into();
        let client = StdioMcpClient::connect(&s_name, config).await?;
        let provider_id = if config.is_core {
            "core".to_string()
        } else {
            format!("mcp:{s_name}")
        };
        Ok(Arc::new(Self {
            provider_id,
            server_name: s_name,
            is_core: config.is_core,
            client,
            cached_schemas: RwLock::new(HashMap::new()),
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }))
    }

    /// Resolve relative path arguments against `workspace_cwd` according to the tool's JSON Schema (§3.1).
    pub fn resolve_relative_paths_by_schema(
        arguments: &Value,
        input_schema: &Value,
        workspace_cwd: &Path,
    ) -> Value {
        let Some(arg_obj) = arguments.as_object() else {
            return arguments.clone();
        };
        let properties = input_schema.get("properties").and_then(|p| p.as_object());

        let mut rewritten = arg_obj.clone();
        for (key, val) in arg_obj {
            let Some(s) = val.as_str() else {
                continue;
            };
            let is_path_field = properties
                .and_then(|props| props.get(key))
                .map(|prop_schema| is_schema_property_a_path(key, prop_schema))
                .unwrap_or_else(|| is_likely_path_key(key));

            if is_path_field {
                let candidate = Path::new(s);
                if !s.is_empty() && candidate.is_relative() && !s.contains("://") {
                    let resolved = workspace_cwd.join(candidate);
                    rewritten.insert(
                        key.clone(),
                        Value::String(resolved.to_string_lossy().into_owned()),
                    );
                }
            }
        }
        Value::Object(rewritten)
    }
}

fn is_schema_property_a_path(key: &str, prop_schema: &Value) -> bool {
    if let Some(fmt) = prop_schema.get("format").and_then(|v| v.as_str()) {
        if matches!(fmt, "path" | "file-path" | "filepath" | "directory-path") {
            return true;
        }
    }
    is_likely_path_key(key)
}

fn is_likely_path_key(key: &str) -> bool {
    matches!(
        key,
        "path" | "file_path" | "filepath" | "cwd" | "directory" | "dir" | "target_path"
    )
}

#[async_trait]
impl ToolProvider for McpToolProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn list_tools(&self) -> Result<Vec<ToolDescriptor>, ToolExecutionError> {
        let raw = self
            .client
            .request("tools/list", Some(json!({})), Duration::from_secs(15))
            .await?;
        let parsed: McpListToolsResult = serde_json::from_value(raw)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;

        let mut descriptors = Vec::with_capacity(parsed.tools.len());
        let mut schema_map = HashMap::new();

        for tool in parsed.tools {
            let description = tool.description.unwrap_or_default();
            let desc = if self.is_core {
                ToolDescriptor::new_core(&tool.name, description, tool.input_schema.clone())
            } else {
                ToolDescriptor::new_mcp(
                    &self.server_name,
                    &tool.name,
                    description,
                    tool.input_schema.clone(),
                )
            };
            schema_map.insert(tool.name.clone(), tool.input_schema.clone());
            schema_map.insert(desc.name.clone(), tool.input_schema);
            descriptors.push(desc);
        }

        *self.cached_schemas.write() = schema_map;
        Ok(descriptors)
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        ctx: &ExecutionContext,
    ) -> Result<String, ToolExecutionError> {
        // Map exposed name (`core__read_file` or `mcp__git__commit`) back to raw MCP tool name.
        let raw_tool_name = if self.is_core {
            name.strip_prefix("core__").unwrap_or(name).to_string()
        } else {
            let prefix = format!(
                "mcp__{}__",
                thunder_protocol::sanitize_identifier(&self.server_name)
            );
            name.strip_prefix(&prefix).unwrap_or(name).to_string()
        };

        // For 3rd-party MCP servers, convert relative path arguments using the tool's schema (§3.1).
        let effective_args = if self.is_core {
            arguments
        } else {
            let schema = self
                .cached_schemas
                .read()
                .get(&raw_tool_name)
                .cloned()
                .unwrap_or_else(|| json!({}));
            Self::resolve_relative_paths_by_schema(&arguments, &schema, &ctx.workspace_cwd)
        };

        let call_params = McpToolCallParams {
            name: raw_tool_name,
            arguments: effective_args,
            meta: Some(ContextEnvelopeMeta::from(ctx)),
        };

        let params_val = serde_json::to_value(call_params)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;

        let raw_res = self
            .client
            .request("tools/call", Some(params_val), ctx.timeout)
            .await?;

        let mcp_result: McpCallToolResult = serde_json::from_value(raw_res)
            .map_err(|e| ToolExecutionError::SerializationError(e.to_string()))?;

        let text = mcp_result.combined_text();
        if mcp_result.is_error {
            return Err(ToolExecutionError::NonZeroExit {
                code: 1,
                stderr: strip_ansi_codes(&text),
            });
        }

        // Apply flood protection to MCP responses as well (§5.2).
        let guarded = drain_stream_with_flood_guard(
            std::io::Cursor::new(text.into_bytes()),
            self.max_output_bytes,
            &ctx.session_id,
            "mcp",
        )
        .await
        .map_err(ToolExecutionError::IOError)?;

        Ok(guarded.text)
    }
}
