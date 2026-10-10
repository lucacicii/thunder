//! Tools and completion gates whose work happens in the host process.

use crate::protocol::RuntimeEvent;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use thunder_agent_loop::loop_engine::gate::{CompletionGate, GateRequest, GateVerdict};
use thunder_agent_loop::types::policy::ToolEffect;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Shared registry of tool calls the host has not answered yet.
pub type PendingTools = Arc<Mutex<HashMap<String, oneshot::Sender<(bool, String)>>>>;

/// Shared slot for the in-flight gate question of each run.
pub type PendingGates = Arc<Mutex<HashMap<String, oneshot::Sender<GateVerdict>>>>;

/// Map a host-declared effect string onto the loop's taxonomy.
pub fn parse_effect(raw: Option<&str>) -> ToolEffect {
    match raw.map(str::to_ascii_lowercase).as_deref() {
        Some("read") => ToolEffect::Read,
        Some("write") => ToolEffect::Write,
        Some("exec") => ToolEffect::Exec,
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
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(call_id.clone(), tx);

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
            self.pending.lock().await.remove(&call_id);
            return Err("the host is gone; tool call could not be delivered".to_string());
        }

        // A cancelled run unblocks the loop by dropping the sender; the loop
        // has already stopped caring, but this task must not leak.
        match rx.await {
            Ok((true, output)) => Ok(output),
            Ok((false, output)) => Err(output),
            Err(_) => Err("the host dropped the tool call without answering".to_string()),
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
        // only verdict that lets the run end.
        rx.await.unwrap_or(GateVerdict::Pass)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::RuntimeRequest;

    #[test]
    fn declared_effects_map_onto_the_loop_taxonomy() {
        assert_eq!(parse_effect(Some("read")), ToolEffect::Read);
        assert_eq!(parse_effect(Some("WRITE")), ToolEffect::Write);
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
