//! Tools and completion gates whose work happens in the host process.

use crate::protocol::RuntimeEvent;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::gate::{CompletionGate, GateRequest, GateVerdict};
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_types::tool::ToolDefinition;
use tokio::sync::{mpsc, oneshot, Mutex};

/// Shared registry of tool calls the host has not answered yet, keyed by
/// `(run_id, call_id)`.
///
/// The host names the run in every `tool_result` frame; keying on the call id
/// alone would let a frame addressed to one run answer a call in another (and
/// would collide outright if a host reuses ids across runs).
pub type PendingTools = Arc<Mutex<HashMap<(String, String), oneshot::Sender<(bool, String)>>>>;

/// Shared slot for the in-flight gate question of each run.
pub type PendingGates = Arc<Mutex<HashMap<String, oneshot::Sender<GateVerdict>>>>;

/// Map a host-declared effect string onto the loop's taxonomy.
pub fn parse_effect(raw: Option<&str>) -> ToolEffect {
    match raw.map(str::to_ascii_lowercase).as_deref() {
        Some("read") => ToolEffect::Read,
        Some("write") => ToolEffect::Write,
        Some("exec") => ToolEffect::Exec,
        Some("plan_artifact") => ToolEffect::PlanArtifact,
        // Unclassified: the loop treats it as a mutation, which is the safe
        // assumption for a tool the host did not describe.
        _ => ToolEffect::Other,
    }
}

/// A tool the loop advertises to the model but never executes itself: every
/// call is forwarded to the host, which owns the business logic.
pub struct RemoteTool {
    run_id: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
    effect: ToolEffect,
    out: mpsc::UnboundedSender<RuntimeEvent>,
    pending: PendingTools,
}

impl RemoteTool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        run_id: String,
        name: String,
        description: String,
        parameters: serde_json::Value,
        effect: ToolEffect,
        out: mpsc::UnboundedSender<RuntimeEvent>,
        pending: PendingTools,
    ) -> Self {
        Self {
            run_id,
            name,
            description,
            parameters,
            effect,
            out,
            pending,
        }
    }
}

#[async_trait]
impl AgentTool for RemoteTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            self.name.clone(),
            self.description.clone(),
            self.parameters.clone(),
        )
    }

    fn effect(&self) -> ToolEffect {
        self.effect
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let call_id = ctx.tool_call_id.clone();
        let key = (self.run_id.clone(), call_id.clone());
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key.clone(), tx);

        if self
            .out
            .send(RuntimeEvent::ToolCall {
                run_id: self.run_id.clone(),
                call_id: call_id.clone(),
                name: self.name.clone(),
                arguments: args,
            })
            .is_err()
        {
            self.pending.lock().await.remove(&key);
            return Err("the host is gone; tool call could not be delivered".to_string());
        }

        // A cancelled run unblocks the loop by dropping the sender; the loop
        // has already stopped caring, but this task must not leak. Cancelling
        // here — rather than waiting for the host — also means a host that
        // never answers cannot hold the task past the run it belongs to.
        tokio::select! {
            biased;
            _ = ctx.cancellation_token.cancelled() => {
                self.pending.lock().await.remove(&key);
                Err("run cancelled while the host was executing the tool".to_string())
            }
            answered = rx => match answered {
                Ok((true, output)) => Ok(output),
                Ok((false, output)) => Err(output),
                Err(_) => Err("the host dropped the tool call without answering".to_string()),
            },
        }
    }
}

/// Asks the host whether a finished run may finish.
pub struct RemoteGate {
    run_id: String,
    out: mpsc::UnboundedSender<RuntimeEvent>,
    pending: PendingGates,
}

impl RemoteGate {
    pub fn new(
        run_id: String,
        out: mpsc::UnboundedSender<RuntimeEvent>,
        pending: PendingGates,
    ) -> Self {
        Self {
            run_id,
            out,
            pending,
        }
    }
}

#[async_trait]
impl CompletionGate for RemoteGate {
    async fn verify(&self, request: GateRequest) -> GateVerdict {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(self.run_id.clone(), tx);

        if self
            .out
            .send(RuntimeEvent::GateRequest {
                run_id: self.run_id.clone(),
                round: request.round,
                final_text: request.final_text,
            })
            .is_err()
        {
            self.pending.lock().await.remove(&self.run_id);
            return GateVerdict::Pass;
        }

        // A host that never answers must not wedge the loop; passing is the
        // only verdict that lets the run end. Cancelling must not wait for the
        // answer either — the loop is already unwinding, and passing is the
        // verdict that lets it unwind without a spurious retry.
        let verdict = tokio::select! {
            biased;
            _ = request.cancellation_token.cancelled() => GateVerdict::Pass,
            answered = rx => answered.unwrap_or(GateVerdict::Pass),
        };
        self.pending.lock().await.remove(&self.run_id);
        verdict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RuntimeRequest;
    use std::time::Duration;
    use thunder_agent_loop::types::message::ChatMessage;
    use tokio_util::sync::CancellationToken;

    fn tool_context(token: CancellationToken) -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: "call_1".to_string(),
            turn: 1,
            cancellation_token: token,
            caller: None,
            route: Some("run_1".to_string()),
            effect: None,
            event_sink: None,
            ui: None,
            depth: 0,
            max_depth: 3,
            call_chain: Vec::new(),
            sub_invoker: None,
            progress_reporter: None,
        }
    }

    #[tokio::test]
    async fn a_cancelled_run_stops_waiting_for_the_host_tool_result() {
        let (out, _frames) = mpsc::unbounded_channel::<RuntimeEvent>();
        let pending: PendingTools = Arc::new(Mutex::new(HashMap::new()));
        let tool = RemoteTool::new(
            "run_1".to_string(),
            "read_file".to_string(),
            "read".to_string(),
            serde_json::json!({"type": "object"}),
            ToolEffect::Read,
            out,
            pending.clone(),
        );

        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tool.execute(serde_json::json!({}), &tool_context(token)),
        )
        .await
        .expect("cancellation must unblock the tool, not the test");

        assert!(result.is_err(), "a cancelled call is not a successful read");
        assert!(
            pending.lock().await.is_empty(),
            "the pending entry must not leak past the run"
        );
    }

    #[tokio::test]
    async fn a_tool_result_is_matched_by_run_and_call_id() {
        let (out, mut frames) = mpsc::unbounded_channel::<RuntimeEvent>();
        let pending: PendingTools = Arc::new(Mutex::new(HashMap::new()));
        let tool = RemoteTool::new(
            "run_1".to_string(),
            "read_file".to_string(),
            "read".to_string(),
            serde_json::json!({"type": "object"}),
            ToolEffect::Read,
            out,
            pending.clone(),
        );

        let handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({}),
                &tool_context(CancellationToken::new()),
            )
            .await
        });

        let frame = frames.recv().await.expect("a tool call frame");
        let (run_id, call_id) = match frame {
            RuntimeEvent::ToolCall {
                run_id, call_id, ..
            } => (run_id, call_id),
            other => panic!("unexpected frame: {other:?}"),
        };
        assert_eq!(run_id, "run_1");

        let sender = pending
            .lock()
            .await
            .remove(&(run_id, call_id))
            .expect("pending entry keyed by (run, call)");
        sender.send((true, "contents".to_string())).unwrap();

        assert_eq!(handle.await.unwrap().unwrap(), "contents");
    }

    #[tokio::test]
    async fn a_cancelled_run_does_not_wait_for_the_gate() {
        let (out, _frames) = mpsc::unbounded_channel::<RuntimeEvent>();
        let pending: PendingGates = Arc::new(Mutex::new(HashMap::new()));
        let gate = RemoteGate::new("run_1".to_string(), out, pending.clone());

        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            canceller.cancel();
        });

        let verdict = tokio::time::timeout(
            Duration::from_secs(2),
            gate.verify(GateRequest {
                turn: 1,
                round: 1,
                final_text: "done".to_string(),
                messages: vec![ChatMessage::user("go")],
                stats: Default::default(),
                cancellation_token: token,
            }),
        )
        .await
        .expect("cancellation must unblock the gate");

        assert_eq!(
            verdict,
            GateVerdict::Pass,
            "an unwinding run passes rather than retrying"
        );
        assert!(
            pending.lock().await.is_empty(),
            "the gate slot must not leak past the run"
        );
    }

    #[test]
    fn declared_effects_map_onto_the_loop_taxonomy() {
        assert_eq!(parse_effect(Some("read")), ToolEffect::Read);
        assert_eq!(parse_effect(Some("WRITE")), ToolEffect::Write);
        assert_eq!(
            parse_effect(Some("plan_artifact")),
            ToolEffect::PlanArtifact
        );
        assert_eq!(parse_effect(Some("exec")), ToolEffect::Exec);
        // Unknown or absent: a mutation, never "harmless".
        assert_eq!(parse_effect(Some("something-else")), ToolEffect::Other);
        assert_eq!(parse_effect(None), ToolEffect::Other);
    }

    #[test]
    fn tool_result_requests_deserialize_without_optional_noise() {
        let req: RuntimeRequest = serde_json::from_str(
            r#"{"method":"tool_result","run_id":"r1","call_id":"c1","ok":true,"output":"42"}"#,
        )
        .expect("minimal tool_result");
        match req {
            RuntimeRequest::ToolResult {
                run_id,
                call_id,
                ok,
                output,
                ..
            } => {
                assert_eq!(run_id, "r1");
                assert_eq!(call_id, "c1");
                assert!(ok);
                assert_eq!(output, "42");
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }

    #[test]
    fn start_defaults_are_conservative() {
        let req: RuntimeRequest = serde_json::from_str(
            r#"{"method":"start","run_id":"r1","model":{"provider":"p","id":"m","name":"m","api":"openai-completions","baseUrl":"https://example.invalid/v1"}}"#,
        )
        .expect("minimal start");
        match req {
            RuntimeRequest::Start {
                max_turns,
                request_timeout_ms,
                gate,
                messages,
                tools,
                ..
            } => {
                assert!(max_turns.is_none(), "loop default applies");
                assert_eq!(request_timeout_ms, 60_000);
                assert!(gate.is_none(), "no gate unless the host asks");
                assert!(messages.is_empty());
                assert!(tools.is_empty(), "no tools until the host declares them");
            }
            other => panic!("unexpected request: {other:?}"),
        }
    }
}
