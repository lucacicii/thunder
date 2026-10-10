//! Run lifecycle: one loop per `run_id`, all output funnelled through one writer.

use crate::protocol::{GateSpec, RemoteToolSpec, RuntimeEvent, RuntimeRequest};
use crate::remote::{parse_effect, PendingGates, PendingTools, RemoteGate, RemoteTool};
use std::collections::HashMap;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::gate::GateVerdict;
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::{AgentLoop, ContextInput};
use thunder_agent_providers::RpiAiClient;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Bumped when the wire format changes in a way a host must notice.
pub const PROTOCOL_VERSION: &str = "1";

pub struct Runtime {
    out: mpsc::UnboundedSender<RuntimeEvent>,
    pending_tools: PendingTools,
    pending_gates: PendingGates,
    runs: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Flipped on shutdown so the writer drains and flushes before exiting.
    closing: tokio::sync::watch::Sender<bool>,
}

impl Runtime {
    pub fn new() -> Self {
        let (out, rx) = mpsc::unbounded_channel::<RuntimeEvent>();
        let (closing, closed) = tokio::sync::watch::channel(false);
        tokio::spawn(writer_task(rx, closed));
        Self {
            out,
            pending_tools: Arc::new(Mutex::new(HashMap::new())),
            pending_gates: Arc::new(Mutex::new(HashMap::new())),
            runs: Arc::new(Mutex::new(HashMap::new())),
            closing,
        }
    }

    fn emit(&self, event: RuntimeEvent) {
        // A closed channel means the writer is gone and the process is exiting;
        // dropping the frame is the only option left.
        let _ = self.out.send(event);
    }

    /// Report a protocol-level failure that belongs to no run.
    pub fn emit_error(&self, message: impl Into<String>) {
        self.emit(RuntimeEvent::Error {
            run_id: None,
            message: message.into(),
        });
    }

    pub async fn reply(&self, id: Option<String>, ok: bool, error: Option<String>) {
        self.emit(RuntimeEvent::Response {
            id,
            ok,
            version: None,
            error,
        });
    }

    pub async fn handle(&self, request: RuntimeRequest) {
        match request {
            RuntimeRequest::Ping { id } => {
                self.emit(RuntimeEvent::Response {
                    id,
                    ok: true,
                    version: Some(PROTOCOL_VERSION.to_string()),
                    error: None,
                });
            }
            RuntimeRequest::Start {
                id,
                run_id,
                model,
                system_prompt,
                messages,
                tools,
                max_turns,
                temperature,
                thinking_level,
                request_timeout_ms,
                session_id,
                gate,
            } => {
                self.start_run(
                    id,
                    run_id,
                    model,
                    system_prompt,
                    messages,
                    tools,
                    max_turns,
                    temperature,
                    thinking_level,
                    request_timeout_ms,
                    session_id,
                    gate,
                )
                .await;
            }
            RuntimeRequest::ToolResult {
                id,
                run_id,
                call_id,
                ok,
                output,
            } => {
                let sender = self.pending_tools.lock().await.remove(&call_id);
                match sender {
                    Some(sender) => {
                        // A closed receiver means the run was cancelled between
                        // the call and this answer.
                        let _ = sender.send((ok, output));
                        self.reply(id, true, None).await;
                    }
                    None => {
                        self.reply(
                            id,
                            false,
                            Some(format!(
                                "no pending tool call `{call_id}` for run `{run_id}`"
                            )),
                        )
                        .await;
                    }
                }
            }
            RuntimeRequest::GateResult {
                id,
                run_id,
                verdict,
                feedback,
            } => {
                let parsed = match verdict.as_str() {
                    "pass" => Some(GateVerdict::Pass),
                    "retry" => Some(GateVerdict::Retry {
                        feedback: feedback.unwrap_or_default(),
                    }),
                    "fail" => Some(GateVerdict::Fail {
                        reason: feedback.unwrap_or_else(|| "rejected by host".to_string()),
                    }),
                    _ => None,
                };
                match parsed {
                    None => {
                        self.reply(id, false, Some(format!("unknown verdict `{verdict}`")))
                            .await;
                    }
                    Some(verdict) => {
                        let sender = self.pending_gates.lock().await.remove(&run_id);
                        match sender {
                            Some(sender) => {
                                let _ = sender.send(verdict);
                                self.reply(id, true, None).await;
                            }
                            None => {
                                self.reply(
                                    id,
                                    false,
                                    Some(format!("no pending gate request for run `{run_id}`")),
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            RuntimeRequest::Cancel { id, run_id } => {
                let token = self.runs.lock().await.remove(&run_id);
                match token {
                    Some(token) => {
                        token.cancel();
                        self.reply(id, true, None).await;
                    }
                    None => {
                        self.reply(id, false, Some(format!("no such run `{run_id}`")))
                            .await;
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_run(
        &self,
        id: Option<String>,
        run_id: String,
        model: thunder_agent_providers::ModelDescriptor,
        system_prompt: Option<String>,
        messages: Vec<thunder_agent_loop::ChatMessage>,
        tools: Vec<RemoteToolSpec>,
        max_turns: Option<usize>,
        temperature: Option<f32>,
        thinking_level: Option<String>,
        request_timeout_ms: u64,
        session_id: Option<String>,
        gate: Option<GateSpec>,
    ) {
        {
            let runs = self.runs.lock().await;
            if runs.contains_key(&run_id) {
                drop(runs);
                self.reply(id, false, Some(format!("run `{run_id}` already exists")))
                    .await;
                return;
            }
        }

        let mut config = AgentConfig::new(model.id.clone());
        config.system_prompt = system_prompt;
        if let Some(turns) = max_turns {
            config.max_turns = Some(turns);
        }
        config.temperature = temperature;
        config.thinking_level = thinking_level;
        config.request_timeout_ms = request_timeout_ms;
        config.session_id = session_id;

        // A dialect this build cannot stream fails the run here, with the reason
        // in the reply — the host has no other way to learn why nothing streamed.
        let client = match RpiAiClient::from_descriptor(&model, request_timeout_ms) {
            Ok((client, notes)) => {
                if !notes.is_empty() {
                    tracing::warn!(
                        model = %model.id,
                        notes = ?notes.describe(),
                        "model descriptor lost fields the stream client cannot carry"
                    );
                }
                client
            }
            Err(err) => {
                self.reply(id, false, Some(format!("model `{}`: {err}", model.id)))
                    .await;
                return;
            }
        };
        let mut agent = AgentLoop::new(config)
            .with_id(run_id.clone())
            .with_custom_client(Arc::new(client));

        for spec in tools {
            agent.register_tool(Arc::new(RemoteTool::new(
                run_id.clone(),
                spec.name,
                spec.description,
                spec.parameters,
                parse_effect(spec.effect.as_deref()),
                self.out.clone(),
                self.pending_tools.clone(),
            )));
        }

        if let Some(gate) = gate {
            agent = agent.with_completion_gate(
                Arc::new(RemoteGate::new(
                    run_id.clone(),
                    self.out.clone(),
                    self.pending_gates.clone(),
                )),
                gate.max_rounds,
            );
        }

        let token = CancellationToken::new();
        self.runs.lock().await.insert(run_id.clone(), token.clone());

        let mut handle = match agent.start(ContextInput::Messages(messages), Some(token.clone())) {
            Ok(handle) => handle,
            Err(err) => {
                self.runs.lock().await.remove(&run_id);
                error!(run_id = %run_id, error = %err, "run failed to start");
                self.reply(id, false, Some(err.to_string())).await;
                return;
            }
        };

        self.reply(id, true, None).await;

        // Stream the loop's own events, verbatim.
        if let Some(mut events) = handle.take_events() {
            let out = self.out.clone();
            let stream_run = run_id.clone();
            tokio::spawn(async move {
                while let Some(observed) = events.recv().await {
                    let _ = out.send(RuntimeEvent::Observation {
                        run_id: stream_run.clone(),
                        event: observed.event,
                    });
                }
            });
        }

        let out = self.out.clone();
        let runs = self.runs.clone();
        let gates = self.pending_gates.clone();
        tokio::spawn(async move {
            let settled = handle.join().await;
            runs.lock().await.remove(&run_id);
            gates.lock().await.remove(&run_id);
            match settled {
                Ok(result) => {
                    info!(run_id = %run_id, reason = ?result.finish_reason, "run finished");
                    let _ = out.send(RuntimeEvent::RunFinished {
                        run_id,
                        finish_reason: result.finish_reason,
                        final_content: result.final_content,
                        messages: result.messages,
                        stats: result.stats,
                    });
                }
                Err(err) => {
                    error!(run_id = %run_id, error = %err, "run errored");
                    let _ = out.send(RuntimeEvent::Error {
                        run_id: Some(run_id),
                        message: err.to_string(),
                    });
                }
            }
        });
    }

    /// Cancel every live run; called on stdin EOF.
    pub async fn shutdown(&self) {
        let mut runs = self.runs.lock().await;
        for (_, token) in runs.drain() {
            token.cancel();
        }
        drop(runs);
        // Tell the writer to flush whatever is still queued before it stops.
        let _ = self.closing.send(true);
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

/// Single owner of stdout, so frames never interleave mid-line.
async fn writer_task(
    mut rx: mpsc::UnboundedReceiver<RuntimeEvent>,
    mut closing: tokio::sync::watch::Receiver<bool>,
) {
    let mut stdout = tokio::io::stdout();
    loop {
        tokio::select! {
            maybe = rx.recv() => match maybe {
                Some(frame) => write_frame(&mut stdout, &frame).await,
                None => break,
            },
            _ = closing.changed() => {
                if *closing.borrow() {
                    // Drain what is already queued: a reply written just before
                    // EOF is still a reply the host is waiting for.
                    while let Ok(frame) = rx.try_recv() {
                        write_frame(&mut stdout, &frame).await;
                    }
                    break;
                }
            }
        }
    }
    let _ = stdout.flush().await;
}

pub(crate) async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(w: &mut W, frame: &RuntimeEvent) {
    match serde_json::to_string(frame) {
        Ok(line) => {
            let _ = w.write_all(line.as_bytes()).await;
            let _ = w.write_all(b"\n").await;
            let _ = w.flush().await;
        }
        Err(err) => error!(error = %err, "failed to serialize frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_are_single_newline_terminated_json_lines() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &RuntimeEvent::Response {
                id: Some("1".to_string()),
                ok: true,
                version: Some(PROTOCOL_VERSION.to_string()),
                error: None,
            },
        )
        .await;
        let text = String::from_utf8(buf).unwrap();
        assert!(text.ends_with('\n'));
        assert_eq!(text.matches('\n').count(), 1, "one frame is one line");
        let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(value["type"], "response");
        assert_eq!(value["ok"], true);
    }

    #[tokio::test]
    async fn run_finished_carries_the_transcript_back_to_the_host() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(
            &mut buf,
            &RuntimeEvent::RunFinished {
                run_id: "r1".to_string(),
                finish_reason: thunder_agent_loop::FinishReason::Done,
                final_content: Some("all done".to_string()),
                messages: vec![thunder_agent_loop::ChatMessage::user("hi")],
                stats: Default::default(),
            },
        )
        .await;
        let value: serde_json::Value =
            serde_json::from_str(String::from_utf8(buf).unwrap().trim()).unwrap();
        assert_eq!(value["type"], "run_finished");
        assert_eq!(value["finish_reason"], "done");
        assert_eq!(value["messages"].as_array().unwrap().len(), 1);
    }
}
