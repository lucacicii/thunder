//! Shared fixtures for the pack's integration tests.
//!
//! Assembly mirrors what a host does now: a baseline pipeline (the judge lives
//! in the executor's funnel, not in a layer) plus whatever gate the test wants
//! to be judged by.

// Compiled once per test binary, and each binary uses a different subset.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::gate::{GateVerdict, ToolGate, ToolGateRequest};
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_policy::ui::HostUi;

/// The executor as a host assembles it: registry, pipeline, optional gate,
/// optional panel.
#[allow(clippy::too_many_arguments)]
pub fn executor(
    registry: ToolRegistry,
    gate: Option<Arc<dyn ToolGate>>,
    ui: Option<Arc<dyn HostUi>>,
) -> ToolExecutor {
    let mut executor = ToolExecutor::new(registry);
    if let Some(gate) = gate {
        executor = executor.with_tool_gate(Some(gate));
    }
    if let Some(ui) = ui {
        executor = executor.with_host_ui(ui);
    }
    executor
}

/// A gate whose only knob is which effects it asks about. Everything else it
/// allows — which is the shape of a mode, without the host's opinions.
pub struct AskGate {
    pub writes: bool,
    pub exec: bool,
    pub reads: bool,
}

impl AskGate {
    /// The `ask` mode: reads pass, changes ask.
    pub fn mutations() -> Arc<Self> {
        Arc::new(Self {
            writes: true,
            exec: true,
            reads: false,
        })
    }
    /// The `yolo` shape: asks nothing. (Distinguish it from *no gate at all*,
    /// which is the same verdict by a different route — both are tested.)
    pub fn never() -> Arc<Self> {
        Arc::new(Self {
            writes: false,
            exec: false,
            reads: false,
        })
    }
    /// Ask about absolutely everything.
    pub fn always() -> Arc<Self> {
        Arc::new(Self {
            writes: true,
            exec: true,
            reads: true,
        })
    }
}

impl ToolGate for AskGate {
    fn decide(&self, request: &ToolGateRequest<'_>) -> GateVerdict {
        let asks = match request.effect {
            ToolEffect::Read => self.reads,
            ToolEffect::PlanArtifact => false, // the plan artifact is not a workspace change
            ToolEffect::Write | ToolEffect::Other => self.writes,
            ToolEffect::Exec => self.exec,
        };
        if !asks {
            return GateVerdict::Allow;
        }
        GateVerdict::Ask(thunder_agent_policy::gate::ApprovalPrompt {
            title: match request.effect {
                ToolEffect::Exec => format!("Run {}", request.tool),
                ToolEffect::Write | ToolEffect::PlanArtifact => {
                    format!("Write {}", request.tool)
                }
                ToolEffect::Read => format!("Read {}", request.tool),
                ToolEffect::Other => format!("Use {}", request.tool),
            },
            detail: thunder_agent_types::policy::describe_call(request.arguments),
            allow_always: false,
        })
    }

    fn label(&self) -> &str {
        "ask-gate"
    }
}

/// Denies everything, like a host that has decided this session may not change
/// a thing.
pub struct DenyAll;

impl ToolGate for DenyAll {
    fn decide(&self, _request: &ToolGateRequest<'_>) -> GateVerdict {
        GateVerdict::Deny {
            reason: "denied by the test gate".to_string(),
        }
    }

    fn label(&self) -> &str {
        "deny-gate"
    }
}

/// A workspace temp dir, canonicalized so scope comparisons line up.
pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("thunder_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().unwrap_or(dir)
}
