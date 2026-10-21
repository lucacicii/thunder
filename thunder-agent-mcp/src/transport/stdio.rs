use crate::config::McpServerConfig;
use crate::error::McpError;
use crate::protocol::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::transport::McpTransport;
use async_trait::async_trait;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

/// Wait for one request's reply when a server declares no timeout of its own.
///
/// A silent server must not hold a tool call — and its pending-table entry —
/// open forever. Two minutes is a ceiling no ordinary MCP request should reach,
/// while still being generous enough that a slow *tool* is not mistaken for a
/// hang. `McpServerConfig::timeout_ms` overrides it per server, and `Some(0)`
/// opts out entirely for a tool that is legitimately unbounded.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Default ceiling on one inbound JSON-RPC frame.
///
/// The reader used to accumulate a whole line with no bound, so one server
/// answering with a 100 MB payload put 100 MB on the heap before anything could
/// object. The limit is per frame and enforced *while* reading: the overflow is
/// consumed and discarded, never stored.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// The in-flight request table: request id -> the channel its reply arrives on.
#[derive(Clone)]
struct PendingMap(Arc<StdMutex<HashMap<String, oneshot::Sender<JsonRpcResponse>>>>);

impl PendingMap {
    fn new() -> Self {
        Self(Arc::new(StdMutex::new(HashMap::new())))
    }

    /// A poisoned lock is recovered from rather than propagated: only an insert
    /// or a remove can poison it, and neither leaves the map inconsistent, so
    /// one unrelated panic must not take the whole transport down.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<JsonRpcResponse>>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn insert(&self, key: String, tx: oneshot::Sender<JsonRpcResponse>) {
        self.lock().insert(key, tx);
    }

    /// Take the reply channel for `key`, if the request is still in flight.
    fn take(&self, key: &str) -> Option<oneshot::Sender<JsonRpcResponse>> {
        self.lock().remove(key)
    }

    fn len(&self) -> usize {
        self.lock().len()
    }
    /// Forget every in-flight request, resolving each waiter with a dropped
    /// channel. For when the server is gone and no reply can arrive.
    fn clear(&self) {
        self.lock().clear();
    }
}

/// Unregisters its request when it goes out of scope.
///
/// Every exit from `send_request` — a serialization failure, a dead stdin, a
/// timeout, a reply that arrived — must unregister the request. While that was
/// done by hand on each path, the paths that timed out had no removal at all: a
/// server that never answered leaked one table entry per call, forever.
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

/// One inbound frame, or the fact that it was too large to keep.
enum Frame {
    Line(String),
    /// The frame was consumed and discarded; `bytes` is how much arrived.
    Oversized {
        bytes: usize,
    },
}

/// Read one newline-terminated frame, keeping at most `max` bytes of it.
///
/// The point is what does *not* happen: once the frame passes `max`, the rest is
/// consumed from the socket without being appended, so an oversized payload
/// costs nothing beyond the buffer that was already full. Returning
/// `Frame::Oversized` rather than an error keeps the connection usable — the
/// next frame may well be a normal reply.
async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<Frame>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut oversized = false;
    let mut total = 0usize;

    loop {
        let (consumed, found) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                // EOF: whatever is buffered is the last frame, if any.
                if total == 0 && !oversized {
                    return Ok(None);
                }
                break;
            }

            let newline = available.iter().position(|b| *b == b'\n');
            let take = newline.unwrap_or(available.len());

            total += take;
            if !oversized {
                if buf.len() + take > max {
                    oversized = true;
                } else {
                    buf.extend_from_slice(&available[..take]);
                }
            }

            (take + usize::from(newline.is_some()), newline.is_some())
        };
        reader.consume(consumed);
        if found {
            break;
        }
    }

    if oversized {
        return Ok(Some(Frame::Oversized { bytes: total }));
    }
    Ok(Some(Frame::Line(
        String::from_utf8_lossy(&buf).into_owned(),
    )))
}

impl Drop for PendingEntry {
    fn drop(&mut self) {
        // On the success path the reader task already took this entry; `remove`
        // is idempotent, so the two cannot collide.
        self.map.take(&self.key);
    }
}
pub struct StdioTransport {
    server_name: String,
    stdin_tx: mpsc::Sender<String>,
    pending_requests: PendingMap,
    is_alive: Arc<AtomicBool>,
    /// The spawned server process, held for as long as the transport lives.
    ///
    /// This must be stored, not a local: `kill_on_drop(true)` is set on the
    /// `Command`, so the `Child` handle *is* the server's lifetime. Dropping it
    /// kills the process — which is what makes `close()` and a dropped
    /// transport both shut the server down, and what made the previous
    /// never-stored handle SIGKILL every server the instant `spawn` returned.
    child: Arc<Mutex<Option<Child>>>,
    /// Bound on one request round trip; `None` only when the server's own config
    /// asked for that (`timeout_ms = 0`).
    request_timeout: Option<Duration>,
    /// How many inbound frames were refused for exceeding `max_frame_bytes`.
    ///
    /// A diagnostic with a sharp meaning: every count here is one response that
    /// never reached the heap in full, and every in-flight request at that
    /// moment was failed rather than left to time out.
    oversized_frames: Arc<std::sync::atomic::AtomicU64>,
}

impl StdioTransport {
    /// Requests sent but not yet answered.
    ///
    /// A diagnostic: this is zero at rest, and a value that only grows means
    /// replies are being lost (or a server is answering with ids nobody asked
    /// for). Each in-flight request holds one entry, released on the reply, on a
    /// timeout, and on any error on the way out.
    pub fn pending_len(&self) -> usize {
        self.pending_requests.len()
    }

    /// Frames refused for exceeding the frame limit.
    pub fn oversized_frames(&self) -> u64 {
        self.oversized_frames.load(Ordering::SeqCst)
    }

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
        let pending_requests = PendingMap::new();
        let is_alive = Arc::new(AtomicBool::new(true));
        let max_frame_bytes = config.max_frame_bytes.unwrap_or(DEFAULT_MAX_FRAME_BYTES);
        let oversized_frames = Arc::new(std::sync::atomic::AtomicU64::new(0));

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
        let oversized_for_reader = Arc::clone(&oversized_frames);

        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            loop {
                let frame = match read_frame(&mut reader, max_frame_bytes).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(e) => {
                        warn!(server = %name_for_stdout, error = %e, "MCP stdout read error");
                        break;
                    }
                };

                let line = match frame {
                    Frame::Line(line) => line,
                    Frame::Oversized { bytes } => {
                        oversized_for_reader.fetch_add(1, Ordering::SeqCst);
                        // Nothing can be matched to this frame: the id was in the
                        // part that was discarded. Failing every waiter now is
                        // better than letting each one sit out its timeout.
                        pending_map.clear();
                        warn!(
                            server = %name_for_stdout,
                            bytes,
                            limit = max_frame_bytes,
                            "MCP response exceeded max_frame_bytes; frame discarded and \
                             in-flight requests failed"
                        );
                        continue;
                    }
                };
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

                    if let Some(tx) = pending_map.take(&key) {
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
            child: Arc::new(Mutex::new(Some(child))),
            oversized_frames,
            request_timeout: match config.timeout_ms {
                // An explicit zero is the opt-out: a legitimately unbounded tool
                // keeps its unbounded call, and says so in its config.
                Some(0) => None,
                Some(ms) => Some(Duration::from_millis(ms)),
                None => Some(DEFAULT_REQUEST_TIMEOUT),
            },
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
        // Held until this function returns, so every exit below — a serialization
        // failure, a dead stdin, a timeout, a reply — leaves the table unchanged.
        let _pending = PendingEntry::register(&self.pending_requests, id_key.clone(), tx);

        let serialized = serde_json::to_string(&request)
            .map_err(|e| McpError::SerializationError(e.to_string()))?;

        if let Err(e) = self.stdin_tx.send(serialized).await {
            return Err(McpError::TransportError(format!(
                "Failed to send request to '{}': {e}",
                self.server_name
            )));
        }

        match self.request_timeout {
            Some(limit) => match tokio::time::timeout(limit, rx).await {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(_)) => Err(McpError::TransportError(format!(
                    "MCP response channel dropped for request ID '{id_key}'"
                ))),
                Err(_) => Err(McpError::TransportError(format!(
                    "MCP server '{}' did not answer request '{id_key}' within {}ms",
                    self.server_name,
                    limit.as_millis()
                ))),
            },
            None => match rx.await {
                Ok(resp) => Ok(resp),
                Err(_) => Err(McpError::TransportError(format!(
                    "MCP response channel dropped for request ID '{id_key}'"
                ))),
            },
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

    /// Shut the server down and reap it.
    ///
    /// Idempotent: the handle is taken, so a second call is a no-op and a
    /// transport that was already dropped has nothing left to kill. `Child::kill`
    /// both signals (SIGKILL) and waits, so by the time this returns the process
    /// is gone rather than a zombie.
    async fn close(&self) -> Result<(), McpError> {
        self.is_alive.store(false, Ordering::SeqCst);
        // Nothing is coming back for the requests still parked: dropping their
        // senders resolves each waiter with "channel dropped" instead of leaving
        // it parked until the process exits.
        self.pending_requests.clear();
        if let Some(mut child) = self.child.lock().await.take() {
            // An already-exited child makes `kill` return an error; that is the
            // outcome we wanted, not a failure of the shutdown.
            if let Err(e) = child.kill().await {
                debug!(server = %self.server_name, error = %e, "MCP server was already gone at close");
            } else {
                info!(server = %self.server_name, "MCP server process terminated");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;
    use tokio::io::BufReader;

    async fn frame(input: &[u8], max: usize) -> Option<Frame> {
        let mut reader = BufReader::new(input);
        read_frame(&mut reader, max).await.expect("read")
    }

    #[tokio::test]
    async fn a_normal_frame_is_returned_whole() {
        match frame(b"{\"ok\":true}\nnext\n", 1024).await {
            Some(Frame::Line(line)) => assert_eq!(line, "{\"ok\":true}"),
            other => panic!(
                "expected a line, got {:?}",
                other.map(|f| matches!(f, Frame::Line(_)))
            ),
        }
    }

    #[tokio::test]
    async fn a_frame_at_the_limit_fits() {
        let exact = vec![b'x'; 16];
        let mut input = exact.clone();
        input.push(b'\n');
        match frame(&input, 16).await {
            Some(Frame::Line(line)) => assert_eq!(line.len(), 16),
            _ => panic!("a frame exactly at the limit must fit"),
        }
    }

    /// The whole point of the limit: a payload far larger than the cap is
    /// consumed but never buffered.
    #[tokio::test]
    async fn an_oversized_frame_is_discarded_not_buffered() {
        let mut input = vec![b'x'; 10 * 1024 * 1024];
        input.push(b'\n');
        input.extend_from_slice(b"{\"after\":true}\n");

        let mut reader = BufReader::new(&input[..]);
        let first = read_frame(&mut reader, 4096).await.expect("read");
        match first {
            Some(Frame::Oversized { bytes }) => {
                assert!(
                    bytes >= 10 * 1024 * 1024,
                    "the reader saw the payload: {bytes}"
                );
            }
            Some(Frame::Line(_)) => panic!("an oversized frame must not be returned as a line"),
            None => panic!("the frame must be seen, not silently dropped"),
        }

        // The connection stays usable: the next frame is a normal one.
        match read_frame(&mut reader, 4096).await.expect("read") {
            Some(Frame::Line(line)) => assert_eq!(line, "{\"after\":true}"),
            _ => panic!("the next frame must be readable"),
        }
    }

    #[tokio::test]
    async fn eof_with_no_bytes_is_not_a_frame() {
        assert!(frame(b"", 1024).await.is_none());
    }
}
