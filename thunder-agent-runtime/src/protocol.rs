//! The line-delimited JSON protocol between a host and the runtime.
//!
//! One JSON object per line, in both directions. The runtime owns no product
//! state: it is told which model to use, which tools exist, and — for every
//! tool call — it asks the host to do the work and waits for the answer.
//! That is what makes it usable from any business domain, in any language.

use serde::{Deserialize, Serialize};
use thunder_agent_loop::types::message::ChatMessage;

/// A tool the host is willing to execute on the agent's behalf.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteToolSpec {
    pub name: String,
    pub description: String,
    #[serde(default = "default_parameters")]
    pub parameters: serde_json::Value,
    /// What the tool does, so the loop can judge it: `read`, `write`, `exec`,
    /// or omitted for "unclassified". A host that knows its tools should say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
}

fn default_parameters() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}

/// Optional host-verified completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSpec {
    /// How many times the gate may send the run back before the loop gives up.
    #[serde(default = "default_gate_rounds")]
    pub max_rounds: usize,
}

fn default_gate_rounds() -> usize {
    3
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum RuntimeRequest {
    /// Liveness + protocol version.
    Ping { id: Option<String> },
    /// Start one agent run. Events stream back tagged with `run_id`.
    Start {
        id: Option<String>,
        run_id: String,
        /// pi-ai model descriptor, exactly as the bridge expects it.
        model: thunder_pi_bridge::BridgeModel,
        #[serde(default)]
        system_prompt: Option<String>,
        #[serde(default)]
        messages: Vec<ChatMessage>,
        #[serde(default)]
        tools: Vec<RemoteToolSpec>,
        #[serde(default)]
        max_turns: Option<usize>,
        #[serde(default)]
        temperature: Option<f32>,
        #[serde(default)]
        thinking_level: Option<String>,
        #[serde(default = "default_timeout_ms")]
        request_timeout_ms: u64,
        /// Prompt-cache routing key (one conversation = one key).
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        gate: Option<GateSpec>,
    },
    /// The host's answer to a `tool_call`.
    ToolResult {
        id: Option<String>,
        run_id: String,
        call_id: String,
        #[serde(default)]
        ok: bool,
        #[serde(default)]
        output: String,
    },
    /// The host's verdict on a `gate_request`.
    GateResult {
        id: Option<String>,
        run_id: String,
        /// `pass`, `retry`, or `fail`.
        verdict: String,
        #[serde(default)]
        feedback: Option<String>,
    },
    /// Stop a run.
    Cancel { id: Option<String>, run_id: String },
}

fn default_timeout_ms() -> u64 {
    60_000
}

/// What the runtime sends back. Every frame is one line.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// Reply to a request that has a direct answer.
    Response {
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A raw loop event, forwarded verbatim (tokens, turns, tools, progress).
    Observation {
        run_id: String,
        event: thunder_agent_loop::AgentEvent,
    },
    /// The loop needs the host to run a tool.
    ToolCall {
        run_id: String,
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// The loop finished an answer and wants the host's verdict before stopping.
    GateRequest {
        run_id: String,
        round: usize,
        final_text: String,
    },
    /// The run settled. `messages` is the full transcript for the host to keep.
    RunFinished {
        run_id: String,
        finish_reason: thunder_agent_loop::FinishReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_content: Option<String>,
        messages: Vec<ChatMessage>,
        stats: thunder_agent_loop::AgentStats,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        message: String,
    },
}
