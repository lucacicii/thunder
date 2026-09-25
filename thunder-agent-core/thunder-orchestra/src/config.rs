use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thunder_agent_loop::AgentConfig;

/// How B should launch the units it owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topology {
    /// Single autonomous agent unit for straightforward queries and atomic commands.
    Single,
    /// Units run one after another; later units receive the previous final answer (Planner ➔ Coder).
    Sequential,
    /// All units `start` together; B waits for every handle (Planner + Reviewer).
    Parallel,
    /// Decomposes a large task into distinct subtasks and runs specialized workers concurrently on each slice.
    FanOut,
    /// Autonomous heuristic routing analyzes intent and selects the optimal topology.
    Auto,
}

impl Default for Topology {
    fn default() -> Self {
        Self::Auto
    }
}

/// One A instance B will construct.
#[derive(Debug, Clone)]
pub struct UnitSpec {
    pub id: String,
    pub role: String,
    pub config: AgentConfig,
    pub register_builtins: bool,
    pub system_prompt: Option<String>,
}

impl UnitSpec {
    pub fn new(id: impl Into<String>, role: impl Into<String>, config: AgentConfig) -> Self {
        Self {
            id: id.into(),
            role: role.into(),
            config,
            register_builtins: false,
            system_prompt: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_builtins(mut self) -> Self {
        self.register_builtins = true;
        self
    }
}

#[derive(Debug, Clone)]
pub struct OrchestraConfig {
    pub topology: Topology,
    pub scratch_root: PathBuf,
    pub store_root: PathBuf,
    pub units: Vec<UnitSpec>,
    /// Shared base `AgentConfig` used to build a standalone LLM client for
    /// diagnostics (e.g. `Scheduler::health`). Falls back to `units[0].config`
    /// if unset.
    pub base: Option<AgentConfig>,
    /// Whether to synthesize findings across multiple units into an aggregated report.
    pub synthesize: bool,
    /// Optional dedicated UnitSpec used to run the synthesis aggregator.
    pub synthesizer: Option<UnitSpec>,
}

impl OrchestraConfig {
    pub fn new(topology: Topology) -> Self {
        Self {
            topology,
            scratch_root: default_scratch_root(),
            store_root: PathBuf::from("runs"),
            units: Vec::new(),
            base: None,
            synthesize: false,
            synthesizer: None,
        }
    }

    pub fn with_unit(mut self, spec: UnitSpec) -> Self {
        self.units.push(spec);
        self
    }

    pub fn with_synthesizer(mut self, enabled: bool) -> Self {
        self.synthesize = enabled;
        self
    }

    pub fn with_synthesizer_unit(mut self, spec: UnitSpec) -> Self {
        self.synthesizer = Some(spec);
        self.synthesize = true;
        self
    }

    pub fn with_store_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.store_root = root.into();
        self
    }

    pub fn with_scratch_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.scratch_root = root.into();
        self
    }

    pub fn with_base(mut self, cfg: AgentConfig) -> Self {
        self.base = Some(cfg);
        self
    }
}

fn default_scratch_root() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".thunder").join("orchestra")
    } else {
        std::env::temp_dir().join("thunder-orchestra")
    }
}
