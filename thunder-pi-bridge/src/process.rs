//! Node sidecar process management: spawn, health gate, crash restart and
//! per-request event dispatch over NDJSON/stdio.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thunder_agent_loop::stream::client::LLMStreamChunk;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, info, warn};

/// Where events for a given request id are routed.
enum DispatchTarget {
    Stream(PendingStream),
    Models(oneshot::Sender<Result<Vec<BridgeModelInfo>, String>>),
    Ready(oneshot::Sender<Result<String, String>>),
}

pub struct PendingStream {
    pub tx: mpsc::Sender<Result<LLMStreamChunk, String>>,
    /// Millis of last activity; watchdog uses this for idle timeouts.
    pub last_activity_ms: Arc<AtomicU64>,
}

/// A model catalog entry returned by `list_models` (pi-ai builtin catalog).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeModelInfo {
    pub provider: String,
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub context_window: usize,
    #[serde(default)]
    pub max_tokens: usize,
    #[serde(default)]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
}

struct RunningBridge {
    stdin_tx: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<String, DispatchTarget>>>,
    _child: Child,
}

pub struct PiAiBridge {
    inner: Mutex<Option<RunningBridge>>,
    bridge_dir: PathBuf,
    /// Explicit pi-ai package dir (tests / THUNDER_PI_AI_PATH). `None` lets
    /// bridge.mjs resolve (local node_modules → global scan → auto-install).
    pi_ai_path: Option<PathBuf>,
    ready_timeout: Duration,
    req_counter: AtomicU64,
    /// Monotonic launch counter; bumped every spawn for crash diagnostics.
    generation: AtomicU64,
    last_launch: Mutex<Option<Instant>>,
}

impl PiAiBridge {
    /// Launch a bridge rooted at `bridge_dir`. The runner files
    /// (`bridge.mjs`, `package.json`) are copied there from the crate's
    /// `runner/` directory when missing.
    pub async fn launch(bridge_dir: PathBuf, pi_ai_path: Option<PathBuf>) -> Result<Arc<Self>, String> {
        Self::launch_with_timeout(bridge_dir, pi_ai_path, Duration::from_secs(240)).await
    }

    pub async fn launch_with_timeout(
        bridge_dir: PathBuf,
        pi_ai_path: Option<PathBuf>,
        ready_timeout: Duration,
    ) -> Result<Arc<Self>, String> {
        let bridge = Arc::new(Self {
            inner: Mutex::new(None),
            bridge_dir,
            pi_ai_path,
            ready_timeout,
            req_counter: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            last_launch: Mutex::new(None),
        });
        // Fail fast on obviously broken setups (node missing, files uncopyable).
        bridge.ensure_runner_files().await?;
        bridge.ensure_started().await?;
        Ok(bridge)
    }

    pub fn next_request_id(&self) -> String {
        format!("req-{}", self.req_counter.fetch_add(1, Ordering::Relaxed))
    }

    /// Copy `bridge.mjs` + `package.json` from the crate runner dir into the
    /// bridge install dir. `package.json` is only created when missing (so a
    /// local `node_modules` survives), but `bridge.mjs` is refreshed whenever its
    /// bytes differ. The install dir is long-lived and shared across daemon
    /// builds; without this check a rebuilt bridge would never be picked up.
    async fn ensure_runner_files(&self) -> Result<(), String> {
        let mjs = self.bridge_dir.join("bridge.mjs");
        let pkg = self.bridge_dir.join("package.json");
        if mjs.exists() && pkg.exists() && !self.bridge_stale(&mjs).await {
            return Ok(());
        }
        let src = Self::runner_source_dir()?;
        tokio::fs::create_dir_all(&self.bridge_dir)
            .await
            .map_err(|e| format!("failed to create bridge dir {:?}: {e}", self.bridge_dir))?;
        for file in ["bridge.mjs", "package.json"] {
            let from = src.join(file);
            let to = self.bridge_dir.join(file);
            // bridge.mjs must stay in sync with the daemon build; package.json is
            // user-editable (extra deps) so we only seed it once.
            let should_copy = if file == "bridge.mjs" {
                !to.exists() || self.bridge_stale(&to).await
            } else {
                !to.exists()
            };
            if should_copy {
                tokio::fs::copy(&from, &to)
                    .await
                    .map_err(|e| format!("failed to copy {file} from {from:?} to {to:?}: {e}"))?;
            }
        }
        Ok(())
    }

    /// True when the installed `bridge.mjs` differs from the one shipped with this
    /// daemon build (missing source counts as stale).
    async fn bridge_stale(&self, installed: &Path) -> bool {
        let Ok(src) = Self::runner_source_dir() else {
            return false;
        };
        let shipped = src.join("bridge.mjs");
        match (tokio::fs::read(installed).await, tokio::fs::read(&shipped).await) {
            (Ok(a), Ok(b)) => a != b,
            // If either side is unreadable, fall back to "not stale" so a transient
            // IO error cannot clobber a working install.
            _ => false,
        }
    }

    fn runner_source_dir() -> Result<PathBuf, String> {
        if let Ok(env_dir) = std::env::var("THUNDER_BRIDGE_SRC") {
            let p = PathBuf::from(env_dir);
            if p.join("bridge.mjs").exists() {
                return Ok(p);
            }
        }
        // Compile-time path: works for `cargo run`/`cargo test` from the repo.
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runner");
        if p.join("bridge.mjs").exists() {
            return Ok(p);
        }
        Err(
            "bridge runner sources not found (looked at $THUNDER_BRIDGE_SRC and CARGO_MANIFEST_DIR/runner)"
                .to_string(),
        )
    }

    async fn ensure_started(self: &Arc<Self>) -> Result<(), String> {
        let mut guard = self.inner.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        // Restart backoff: if the previous launch died very recently, wait
        // briefly so a crashing sidecar cannot spin-loop the host.
        {
            let mut last = self.last_launch.lock().await;
            if let Some(t) = *last {
                let since = t.elapsed();
                if since < Duration::from_secs(2) {
                    let wait = Duration::from_secs(2) - since;
                    warn!(wait_ms = wait.as_millis() as u64, "Bridge restarted too quickly; backing off");
                    tokio::time::sleep(wait).await;
                }
            }
            *last = Some(Instant::now());
        }

        let node = Self::find_node().await?;
        let script = self.bridge_dir.join("bridge.mjs");
        if !script.exists() {
            return Err(format!("bridge.mjs missing at {script:?}"));
        }

        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        info!(generation, dir = ?self.bridge_dir, "Starting pi-ai bridge sidecar");

        let mut cmd = Command::new(&node);
        cmd.arg(&script)
            .current_dir(&self.bridge_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(p) = &self.pi_ai_path {
            cmd.env("THUNDER_PI_AI_PATH", p);
        }
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| format!("failed to spawn node: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin on bridge child")?;
        let stdout = child.stdout.take().ok_or("no stdout on bridge child")?;

        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(128);

        // Writer task
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });

        // Ready handshake
        let (ready_tx, ready_rx) = oneshot::channel();
        let pending: Arc<Mutex<HashMap<String, DispatchTarget>>> =
            Arc::new(Mutex::new(HashMap::new()));
        pending
            .lock()
            .await
            .insert("health-0".to_string(), DispatchTarget::Ready(ready_tx));

        // Reader task
        {
            let pending = Arc::clone(&pending);
            let inner_reset = Arc::downgrade(self);
            tokio::spawn(async move {
                Self::read_loop(stdout, pending, inner_reset, generation).await;
            });
        }

        // Stream idle watchdog: fails registered streams that produced no
        // activity within the idle window, so a hung sidecar/provider link can
        // never park an agent turn (or a daemon semaphore slot) forever.
        {
            let pending = Arc::clone(&pending);
            let idle_timeout = Self::stream_idle_timeout();
            tokio::spawn(async move {
                Self::watchdog_loop(pending, idle_timeout, generation).await;
            });
        }

        // Send health probe
        stdin_tx
            .send(r#"{"cmd":"health","id":"health-0"}"#.to_string())
            .await
            .map_err(|_| "bridge stdin closed before health probe".to_string())?;

        let version = tokio::time::timeout(self.ready_timeout, ready_rx)
            .await
            .map_err(|_| format!(
                "pi-ai bridge not ready within {}s (first boot may run `npm install`; check stderr logs)",
                self.ready_timeout.as_secs()
            ))?
            .map_err(|_| "bridge dropped before ready".to_string())?
            .map_err(|e| e)?;

        info!(generation, version = %version, "pi-ai bridge ready");

        *guard = Some(RunningBridge {
            stdin_tx,
            pending,
            _child: child,
        });
        Ok(())
    }

    /// Idle timeout for registered streams. Default 5 minutes (reasoning
    /// models can legitimately stay silent for a while during deep thinking);
    /// override with `THUNDER_BRIDGE_IDLE_TIMEOUT_MS`.
    fn stream_idle_timeout() -> Duration {
        std::env::var("THUNDER_BRIDGE_IDLE_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|ms| *ms >= 1_000)
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(300))
    }

    /// Periodic scan for stale streams. Exits after the pending map has stayed
    /// empty for ~20 minutes of consecutive empty scans (post-restart leftovers
    /// clean themselves up instead of leaking one task per generation).
    async fn watchdog_loop(
        pending: Arc<Mutex<HashMap<String, DispatchTarget>>>,
        idle_timeout: Duration,
        generation: u64,
    ) {
        let mut empty_scans: u32 = 0;
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            let now_ms = now_epoch_ms();
            let failed = scan_once(&pending, now_ms, idle_timeout, generation).await;
            if failed == 0 {
                empty_scans += 1;
                if empty_scans >= 80 {
                    debug!(generation, "watchdog exiting: no pending streams");
                    return;
                }
            } else {
                empty_scans = 0;
            }
        }
    }

    async fn find_node() -> Result<String, String> {
        for bin in ["node", "nodejs"] {
            if Command::new(bin)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false)
            {
                return Ok(bin.to_string());
            }
        }
        Err(
            "Node.js (>= 22) is required: Thunder uses the pi-ai bridge as its LLM transport, \
             but no `node` binary was found in PATH."
                .to_string(),
        )
    }

    async fn read_loop(
        stdout: tokio::process::ChildStdout,
        pending: Arc<Mutex<HashMap<String, DispatchTarget>>>,
        bridge: std::sync::Weak<PiAiBridge>,
        generation: u64,
    ) {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(err) => {
                    debug!(raw = %trimmed, error = %err, "Ignoring non-protocol line from bridge");
                    continue;
                }
            };

            let msg_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let id = value.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();

            if msg_type == "fatal" {
                let message = value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("bridge fatal error")
                    .to_string();
                error!(generation, error = %message, "pi-ai bridge fatal");
                let mut map = pending.lock().await;
                fail_all(&mut map, &message);
                break;
            }

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            match msg_type {
                // Non-terminal stream deltas: keep the registration alive for
                // subsequent events (a stream emits many deltas before done).
                "text_delta" | "reasoning_delta" => {
                    let tx = {
                        let mut map = pending.lock().await;
                        match map.get_mut(&id) {
                            Some(DispatchTarget::Stream(p)) => {
                                p.last_activity_ms.store(now_ms, Ordering::Relaxed);
                                p.tx.clone()
                            }
                            _ => {
                                debug!(id = %id, "delta for unknown/expired request");
                                continue;
                            }
                        }
                    };
                    let delta = value.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                    let chunk = if msg_type == "text_delta" {
                        LLMStreamChunk::Token(delta.to_string())
                    } else {
                        LLMStreamChunk::ReasoningToken(delta.to_string())
                    };
                    let _ = tx.send(Ok(chunk)).await;
                }
                // Terminal events consume the registration.
                "done" | "error" | "models" | "ready" => {
                    let target = pending.lock().await.remove(&id);
                    let Some(target) = target else {
                        debug!(id = %id, msg_type, "terminal event for unknown/expired request");
                        continue;
                    };
                    match target {
                        DispatchTarget::Ready(tx) => {
                            if msg_type == "ready" {
                                let version = value
                                    .get("version")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let _ = tx.send(Ok(version));
                            } else {
                                let _ = tx.send(Err(format!("unexpected health reply: {msg_type}")));
                            }
                        }
                        DispatchTarget::Models(tx) => {
                            if msg_type == "models" {
                                match serde_json::from_value::<Vec<BridgeModelInfo>>(
                                    value.get("models").cloned().unwrap_or_default(),
                                ) {
                                    Ok(models) => {
                                        let _ = tx.send(Ok(models));
                                    }
                                    Err(err) => {
                                        let _ = tx.send(Err(format!("malformed models payload: {err}")));
                                    }
                                }
                            } else {
                                let _ = tx.send(Err(err_text(&value)));
                            }
                        }
                        DispatchTarget::Stream(p) => {
                            p.last_activity_ms.store(now_ms, Ordering::Relaxed);
                            if msg_type == "done" {
                                let _ = p.tx.send(Ok(done_to_chunk(&value))).await;
                            } else {
                                let _ = p.tx.send(Err(err_text(&value))).await;
                            }
                        }
                    }
                }
                other => {
                    debug!(msg_type = other, "Ignoring bridge event");
                }
            }
        }

        // stdout closed: sidecar died. Fail everything and allow a restart.
        let message = "pi-ai bridge process exited".to_string();
        error!(generation, error = %message, "bridge stdout closed");
        {
            let mut map = pending.lock().await;
            fail_all(&mut map, &message);
        }
        if let Some(bridge) = bridge.upgrade() {
            *bridge.inner.lock().await = None;
        }
    }

    /// Register a stream target; the watchdog and reader share `last_activity`.
    /// Starts (or restarts) the sidecar if it is not currently running.
    pub(crate) async fn register_stream(
        self: &Arc<Self>,
        id: String,
        tx: mpsc::Sender<Result<LLMStreamChunk, String>>,
    ) -> Result<Arc<AtomicU64>, String> {
        self.ensure_started().await?;
        let last = Arc::new(AtomicU64::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        ));
        let guard = self.inner.lock().await;
        let running = guard.as_ref().ok_or_else(|| "bridge not running".to_string())?;
        running.pending.lock().await.insert(
            id,
            DispatchTarget::Stream(PendingStream { tx, last_activity_ms: Arc::clone(&last) }),
        );
        Ok(last)
    }

    pub(crate) async fn unregister(self: &Arc<Self>, id: &str) {
        if let Some(running) = self.inner.lock().await.as_ref() {
            running.pending.lock().await.remove(id);
        }
    }

    pub(crate) async fn send_line(self: &Arc<Self>, line: String) -> Result<(), String> {
        // Opportunistic restart: a dead bridge is transparently relaunched.
        self.ensure_started().await?;
        let guard = self.inner.lock().await;
        let running = guard.as_ref().ok_or_else(|| "bridge not running".to_string())?;
        running
            .stdin_tx
            .send(line)
            .await
            .map_err(|e| format!("failed to write to bridge stdin: {e}"))
    }

    /// Send a raw request payload; caller must have registered a target for `id`.
    pub(crate) async fn send_request(self: &Arc<Self>, payload: String) -> Result<(), String> {
        self.send_line(payload).await
    }

    /// Ask the sidecar for the pi-ai builtin model catalog.
    pub async fn list_models(self: &Arc<Self>) -> Result<Vec<BridgeModelInfo>, String> {
        self.ensure_started().await?;
        let id = self.next_request_id();
        let (tx, rx) = oneshot::channel();
        {
            let guard = self.inner.lock().await;
            guard
                .as_ref()
                .expect("bridge started")
                .pending
                .lock()
                .await
                .insert(id.clone(), DispatchTarget::Models(tx));
        }
        let payload = serde_json::json!({ "cmd": "list_models", "id": id }).to_string();
        self.send_request(payload).await?;
        tokio::time::timeout(Duration::from_secs(120), rx)
            .await
            .map_err(|_| "list_models timed out".to_string())?
            .map_err(|_| "list_models responder dropped".to_string())?
    }
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One watchdog scan: unregister streams whose `last_activity_ms` lags beyond
/// `idle_timeout` and deliver a terminal error to their receivers. Returns the
/// number of streams failed. Streams never seen activity (`last == 0`) are
/// exempt — the reader stamps activity on registration-adjacent events.
async fn scan_once(
    pending: &Arc<Mutex<HashMap<String, DispatchTarget>>>,
    now_ms: u64,
    idle_timeout: Duration,
    generation: u64,
) -> usize {
    let idle_ms = idle_timeout.as_millis() as u64;
    let mut stale: Vec<(String, mpsc::Sender<Result<LLMStreamChunk, String>>)> = Vec::new();

    {
        let mut map = pending.lock().await;
        let mut remove_ids: Vec<String> = Vec::new();
        for (id, target) in map.iter() {
            if let DispatchTarget::Stream(p) = target {
                let last = p.last_activity_ms.load(Ordering::Relaxed);
                if last > 0 && now_ms.saturating_sub(last) > idle_ms {
                    remove_ids.push(id.clone());
                    stale.push((id.clone(), p.tx.clone()));
                }
            }
        }
        for id in &remove_ids {
            map.remove(id);
        }
    }

    let failed = stale.len();
    for (id, tx) in stale {
        warn!(
            generation,
            id = %id,
            idle_secs = idle_timeout.as_secs(),
            "watchdog failing idle bridge stream"
        );
        let _ = tx.try_send(Err(format!(
            "bridge stream idle for over {} seconds (watchdog timeout)",
            idle_timeout.as_secs()
        )));
    }

    failed
}

fn fail_all(map: &mut HashMap<String, DispatchTarget>, message: &str) {
    for (_, target) in map.drain() {
        match target {
            DispatchTarget::Ready(tx) => {
                let _ = tx.send(Err(message.to_string()));
            }
            DispatchTarget::Models(tx) => {
                let _ = tx.send(Err(message.to_string()));
            }
            DispatchTarget::Stream(p) => {
                let _ = p.tx.try_send(Err(message.to_string()));
            }
        }
    }
}

fn err_text(value: &serde_json::Value) -> String {
    value
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("bridge stream error")
        .to_string()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct DonePayload {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
    finish_reason: String,
    #[serde(default)]
    usage: RawUsage,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawToolCall {
    id: String,
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawUsage {
    #[serde(default)]
    input: usize,
    #[serde(default)]
    output: usize,
    #[serde(default)]
    cache_read: usize,
    #[serde(default)]
    cache_write: usize,
    reasoning: Option<usize>,
}

fn done_to_chunk(value: &serde_json::Value) -> LLMStreamChunk {
    match serde_json::from_value::<DonePayload>(value.clone()) {
        Ok(done) => LLMStreamChunk::Completed {
            content: done.content.filter(|c| !c.is_empty()),
            tool_calls: done
                .tool_calls
                .into_iter()
                .map(|tc| {
                    thunder_agent_loop::types::message::ToolCall::new_function(
                        tc.id,
                        tc.name,
                        &tc.arguments.to_string(),
                    )
                })
                .collect(),
            finish_reason: done.finish_reason,
            // pi-ai reports `input` as the *uncached* portion only; cache reads and
            // cache writes are separate. Thunder's `prompt_tokens` is the full prompt
            // (the panel derives cache-hit ratios and context occupancy from it), so
            // fold both cache buckets back in. `cached_tokens` stays the read subset.
            prompt_tokens: Some(done.usage.input + done.usage.cache_read + done.usage.cache_write),
            completion_tokens: Some(done.usage.output),
            cached_tokens: Some(done.usage.cache_read),
            reasoning_tokens: done.usage.reasoning,
        },
        Err(err) => {
            // A malformed done payload is a hard stream error for this request.
            LLMStreamChunk::Completed {
                content: Some(format!("bridge protocol error (malformed done payload): {err}")),
                tool_calls: Vec::new(),
                finish_reason: "error".to_string(),
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                reasoning_tokens: None,
            }
        }
    }
}

/// Ensure a directory path for the global bridge install.
pub fn default_bridge_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".thunder").join("bridge")
}

/// One-line helper used by tests: does this dir look like a bridge install?
pub fn is_bridge_dir(dir: &Path) -> bool {
    dir.join("bridge.mjs").exists() && dir.join("package.json").exists()
}

#[cfg(test)]
mod watchdog_tests {
    use super::*;

    #[tokio::test]
    async fn scan_once_fails_only_stale_streams() {
        let pending: Arc<Mutex<HashMap<String, DispatchTarget>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let now = 1_000_000u64;

        // Fresh stream: active a second ago.
        let (tx_fresh, mut rx_fresh) = mpsc::channel(8);
        let fresh_activity = Arc::new(AtomicU64::new(now - 1_000));
        pending.lock().await.insert(
            "fresh".into(),
            DispatchTarget::Stream(PendingStream {
                tx: tx_fresh,
                last_activity_ms: fresh_activity,
            }),
        );

        // Stale stream: last activity 10 minutes ago.
        let (tx_stale, mut rx_stale) = mpsc::channel(8);
        let stale_activity = Arc::new(AtomicU64::new(now - 600_000));
        pending.lock().await.insert(
            "stale".into(),
            DispatchTarget::Stream(PendingStream {
                tx: tx_stale,
                last_activity_ms: stale_activity,
            }),
        );

        // Never-active stream (last == 0): exempt from the watchdog.
        let (tx_zero, mut rx_zero) = mpsc::channel(8);
        let zero_activity = Arc::new(AtomicU64::new(0));
        pending.lock().await.insert(
            "zero".into(),
            DispatchTarget::Stream(PendingStream {
                tx: tx_zero,
                last_activity_ms: zero_activity,
            }),
        );

        let failed = scan_once(&pending, now, Duration::from_secs(300), 7).await;
        assert_eq!(failed, 1, "only the stale stream is failed");

        let err = rx_stale.recv().await.unwrap().unwrap_err();
        assert!(err.contains("watchdog timeout"), "unexpected error: {err}");

        assert!(rx_fresh.try_recv().is_err(), "fresh stream untouched");
        assert!(rx_zero.try_recv().is_err(), "never-active stream untouched");
        assert_eq!(pending.lock().await.len(), 2, "stale entry unregistered");
    }
}
