//! Minimal OpenAI-compatible HTTP server exposing the in-process Qwen2.5 GGUF model.
//!
//! Endpoints (all CORS-enabled so the desktop workbench / a browser page can call it):
//!   GET  /health                -> liveness + model info
//!   GET  /v1/models             -> OpenAI model list
//!   POST /v1/chat/completions   -> OpenAI chat completions (`stream` supported as SSE)
//!
//! There is no new dependency: the server is a hand-rolled HTTP/1.1 implementation on
//! `tokio::net::TcpListener` with `Connection: close` semantics. That is enough for curl,
//! OpenAI SDKs and agent CLIs, and keeps `thunder-memory` free of axum/hyper weight.

use crate::config::MemoryConfig;
use crate::error::MemoryError;
use crate::local_model::{render_chatml, GenOptions, LocalModelEngine};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

/// Options for `thunder-memory serve`.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub host: String,
    pub port: u16,
    /// Hard ceiling for `max_tokens`; a 0.5B model on Metal is fast, but a runaway
    /// request should not block the (single) model lock for minutes.
    pub max_tokens_cap: usize,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8787,
            max_tokens_cap: 1024,
        }
    }
}

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Run the HTTP server forever (until the process is killed).
pub async fn serve(config: MemoryConfig, opts: ServeOptions) -> Result<(), MemoryError> {
    if !config.is_local_model_ready() {
        return Err(MemoryError::Other(format!(
            "local model not found in {:?}: run `thunder-memory download-model` first",
            config.model_dir
        )));
    }

    let gguf = config.local_gguf_path();
    let tokenizer = config.local_tokenizer_path();
    info!("Loading in-process model for HTTP serving...");
    let engine = tokio::task::spawn_blocking(move || LocalModelEngine::load(&gguf, &tokenizer))
        .await
        .map_err(|e| MemoryError::Other(format!("model load task failed: {e}")))??;
    let engine = Arc::new(engine);

    let addr = format!("{}:{}", opts.host, opts.port);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| MemoryError::Other(format!("failed to bind {addr}: {e}")))?;

    let model_id = engine.model_id();
    println!("🌐 thunder-memory serve listening on http://{addr}");
    println!("   model:  {model_id}");
    println!("   POST   http://{addr}/v1/chat/completions");
    println!("   GET    http://{addr}/v1/models");
    info!(addr = %addr, model = %model_id, "OpenAI-compatible endpoint ready");

    let opts = Arc::new(opts);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                warn!("accept() failed: {e}");
                continue;
            }
        };
        let engine = engine.clone();
        let opts = opts.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, engine, opts).await {
                debug!("connection from {peer} ended: {e}");
            }
        });
    }
}

/// A parsed HTTP/1.1 request (headers only, body already split off).
#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Parse the request line + headers. Returns `None` when the request is malformed.
fn parse_request_head(head: &str) -> Option<(String, String, Vec<(String, String)>)> {
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_uppercase();
    let mut path = parts.next()?.to_string();
    // Strip query string; no route uses one.
    if let Some(idx) = path.find('?') {
        path.truncate(idx);
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':')?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    Some((method, path, headers))
}

async fn handle_connection(
    mut stream: TcpStream,
    engine: Arc<LocalModelEngine>,
    opts: Arc<ServeOptions>,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    // Read until the end of the header block.
    let head_end = loop {
        if let Some(pos) = find_double_crlf(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEADER_BYTES {
            return write_response(
                &mut stream,
                431,
                "Request Header Fields Too Large",
                "application/json",
                r#"{"error":{"message":"request headers too large","type":"invalid_request_error"}}"#,
                10,
            )
            .await;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut rest = buf[head_end + 4..].to_vec();

    let (method, path, headers) = match parse_request_head(&head) {
        Some(v) => v,
        None => {
            return write_response(
                &mut stream,
                400,
                "Bad Request",
                "application/json",
                r#"{"error":{"message":"malformed request line","type":"invalid_request_error"}}"#,
                10,
            )
            .await
        }
    };

    let mut req = HttpRequest {
        method,
        path,
        headers,
        body: Vec::new(),
    };

    // Read the body according to Content-Length.
    let content_length: usize = req
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return write_response(
            &mut stream,
            413,
            "Payload Too Large",
            "application/json",
            r#"{"error":{"message":"request body too large","type":"invalid_request_error"}}"#,
            10,
        )
        .await;
    }
    while rest.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        rest.extend_from_slice(&chunk[..n]);
    }
    rest.truncate(content_length);
    req.body = rest;

    route(&mut stream, req, engine, opts).await
}

async fn route(
    stream: &mut TcpStream,
    req: HttpRequest,
    engine: Arc<LocalModelEngine>,
    opts: Arc<ServeOptions>,
) -> std::io::Result<()> {
    // CORS preflight: let browser pages call this endpoint.
    if req.method == "OPTIONS" {
        return write_response(stream, 204, "No Content", "text/plain", "", 0).await;
    }

    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/health") => {
            let body = serde_json::json!({
                "status": "ok",
                "model": engine.model_id(),
                "backend": "candle-metal",
            })
            .to_string();
            write_response(stream, 200, "OK", "application/json", &body, body.len()).await
        }
        ("GET", "/v1/models") | ("GET", "/models") => {
            let body = serde_json::json!({
                "object": "list",
                "data": [{
                    "id": engine.model_id(),
                    "object": "model",
                    "created": now_secs(),
                    "owned_by": "thunder-memory",
                }]
            })
            .to_string();
            write_response(stream, 200, "OK", "application/json", &body, body.len()).await
        }
        ("POST", "/v1/chat/completions") | ("POST", "/chat/completions") => {
            chat_completions(stream, &req, engine, &opts).await
        }
        ("GET", "/") => {
            let body = serde_json::json!({
                "name": "thunder-memory",
                "model": engine.model_id(),
                "endpoints": ["/health", "/v1/models", "/v1/chat/completions"],
            })
            .to_string();
            write_response(stream, 200, "OK", "application/json", &body, body.len()).await
        }
        _ => {
            write_response(
                stream,
                404,
                "Not Found",
                "application/json",
                r#"{"error":{"message":"unknown route","type":"invalid_request_error"}}"#,
                10,
            )
            .await
        }
    }
}

async fn chat_completions(
    stream: &mut TcpStream,
    req: &HttpRequest,
    engine: Arc<LocalModelEngine>,
    opts: &ServeOptions,
) -> std::io::Result<()> {
    let payload: serde_json::Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => {
            let body = serde_json::json!({
                "error": {
                    "message": format!("invalid JSON body: {e}"),
                    "type": "invalid_request_error",
                }
            })
            .to_string();
            return write_response(stream, 400, "Bad Request", "application/json", &body, body.len())
                .await;
        }
    };

    let request = match ChatRequest::from_json(&payload, opts.max_tokens_cap) {
        Ok(r) => r,
        Err(message) => {
            let body = serde_json::json!({
                "error": { "message": message, "type": "invalid_request_error" }
            })
            .to_string();
            return write_response(stream, 400, "Bad Request", "application/json", &body, body.len())
                .await;
        }
    };

    let prompt = render_chatml(&request.messages);
    let gen_opts = GenOptions::chat(request.temperature, request.top_p);
    let prompt_tokens = engine.count_tokens(&prompt);

    debug!(
        messages = request.messages.len(),
        prompt_tokens, max_tokens = request.max_tokens,
        "chat completion request"
    );

    // Model access is serialized by an internal mutex, so run it off the reactor.
    let engine_task = engine.clone();
    let generated = tokio::task::spawn_blocking(move || {
        engine_task.generate_with(&prompt, request.max_tokens, &gen_opts)
    })
    .await;

    let mut text = match generated {
        Ok(Ok(text)) => text,
        Ok(Err(e)) => {
            let body = serde_json::json!({
                "error": { "message": format!("inference failed: {e}"), "type": "server_error" }
            })
            .to_string();
            return write_response(
                stream,
                500,
                "Internal Server Error",
                "application/json",
                &body,
                body.len(),
            )
            .await;
        }
        Err(e) => {
            let body = serde_json::json!({
                "error": { "message": format!("inference task failed: {e}"), "type": "server_error" }
            })
            .to_string();
            return write_response(
                stream,
                500,
                "Internal Server Error",
                "application/json",
                &body,
                body.len(),
            )
            .await;
        }
    };

    text = apply_stop_sequences(text, &request.stop);
    let completion_tokens = engine.count_tokens(&text);
    let id = format!("chatcmpl-{:016x}", rng_seed());
    let created = now_secs();

    if request.stream {
        return stream_chat_completion(stream, &id, &created, &engine, &text, prompt_tokens, completion_tokens).await;
    }

    let body = serde_json::json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": engine.model_id(),
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop",
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
    .to_string();

    write_response(stream, 200, "OK", "application/json", &body, body.len()).await
}

/// The engine generates the whole completion before returning, so streaming is emitted as
/// a single delta followed by `[DONE]`. Clients that request `stream: true` (the OpenAI
/// SDKs, agent CLIs) accept this; they simply get one chunk.
async fn stream_chat_completion(
    stream: &mut TcpStream,
    id: &str,
    created: &u64,
    engine: &LocalModelEngine,
    text: &str,
    prompt_tokens: usize,
    completion_tokens: usize,
) -> std::io::Result<()> {
    let chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": engine.model_id(),
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text }, "finish_reason": null }],
    });
    let final_chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": engine.model_id(),
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    });

    let mut body = String::new();
    body.push_str("data: ");
    body.push_str(&chunk.to_string());
    body.push_str("\n\ndata: ");
    body.push_str(&final_chunk.to_string());
    body.push_str("\n\ndata: [DONE]\n\n");

    write_response(stream, 200, "OK", "text/event-stream", &body, body.len()).await
}

/// Parsed `/v1/chat/completions` request body.
#[derive(Debug, Clone)]
struct ChatRequest {
    messages: Vec<(String, String)>,
    max_tokens: usize,
    temperature: f32,
    top_p: f32,
    stream: bool,
    stop: Vec<String>,
}

impl ChatRequest {
    fn from_json(payload: &serde_json::Value, max_tokens_cap: usize) -> Result<Self, String> {
        let messages_json = payload
            .get("messages")
            .and_then(|m| m.as_array())
            .ok_or_else(|| "`messages` array is required".to_string())?;
        if messages_json.is_empty() {
            return Err("`messages` must not be empty".to_string());
        }

        let mut messages = Vec::with_capacity(messages_json.len());
        for msg in messages_json {
            let role = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string();
            let content = message_text(msg.get("content"));
            messages.push((role, content));
        }

        let max_tokens = payload
            .get("max_tokens")
            .or_else(|| payload.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(256)
            .clamp(1, max_tokens_cap);

        let temperature = payload
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(0.7)
            .clamp(0.0, 2.0);

        let top_p = payload
            .get("top_p")
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(0.95)
            .clamp(0.0, 1.0);

        let stream = payload
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let stop = match payload.get("stop") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => Vec::new(),
        };

        Ok(Self {
            messages,
            max_tokens,
            temperature,
            top_p,
            stream,
            stop,
        })
    }
}

/// OpenAI allows `content` to be a string or an array of content parts.
fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| p.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Truncate the completion at the first occurrence of any stop sequence.
fn apply_stop_sequences(mut text: String, stop: &[String]) -> String {
    for s in stop {
        if s.is_empty() {
            continue;
        }
        if let Some(idx) = text.find(s.as_str()) {
            text.truncate(idx);
        }
    }
    text
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
    content_length: usize,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Headers: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Cache-Control: no-store\r\n\
         \r\n"
    );
    stream.write_all(head.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes()).await?;
    }
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rng_seed() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_head() {
        let head = "POST /v1/chat/completions?x=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 42\r\nContent-Type: application/json";
        let (method, path, headers) = parse_request_head(head).unwrap();
        assert_eq!(method, "POST");
        assert_eq!(path, "/v1/chat/completions");
        let req = HttpRequest {
            method,
            path,
            headers,
            body: Vec::new(),
        };
        assert_eq!(req.header("content-length"), Some("42"));
        assert_eq!(req.header("CONTENT-TYPE"), Some("application/json"));
    }

    #[test]
    fn test_parse_bad_request_line() {
        assert!(parse_request_head("GARBAGE").is_none());
    }

    #[test]
    fn test_find_double_crlf() {
        assert_eq!(find_double_crlf(b"GET / HTTP/1.1\r\n\r\nbody"), Some(14));
        assert!(find_double_crlf(b"GET / HTTP/1.1\r\n").is_none());
    }

    #[test]
    fn test_chat_request_defaults_and_clamps() {
        let payload = serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        let req = ChatRequest::from_json(&payload, 1024).unwrap();
        assert_eq!(req.max_tokens, 256);
        assert!((req.temperature - 0.7).abs() < 1e-6);
        assert!(!req.stream);
        assert_eq!(req.messages[0], ("user".to_string(), "hi".to_string()));

        let payload = serde_json::json!({
            "messages": [{ "role": "user", "content": "hi" }],
            "max_tokens": 99999,
            "temperature": 5.0,
            "stream": true,
            "stop": ["\n\n"]
        });
        let req = ChatRequest::from_json(&payload, 512).unwrap();
        assert_eq!(req.max_tokens, 512);
        assert!((req.temperature - 2.0).abs() < 1e-6);
        assert!(req.stream);
        assert_eq!(req.stop, vec!["\n\n".to_string()]);
    }

    #[test]
    fn test_chat_request_requires_messages() {
        assert!(ChatRequest::from_json(&serde_json::json!({}), 256).is_err());
        assert!(
            ChatRequest::from_json(&serde_json::json!({ "messages": [] }), 256).is_err()
        );
    }

    #[test]
    fn test_message_text_parts() {
        let content = serde_json::json!([
            { "type": "text", "text": "hello" },
            { "type": "text", "text": "world" }
        ]);
        assert_eq!(message_text(Some(&content)), "hello\nworld");
        assert_eq!(message_text(Some(&serde_json::json!("raw"))), "raw");
        assert_eq!(message_text(None), "");
    }

    #[test]
    fn test_apply_stop_sequences() {
        let out = apply_stop_sequences("ab\n\ncd".to_string(), &["\n\n".to_string()]);
        assert_eq!(out, "ab");
        let out = apply_stop_sequences("nothing".to_string(), &["zzz".to_string()]);
        assert_eq!(out, "nothing");
    }
}
