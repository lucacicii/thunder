use crate::config::{OrchestraConfig, Topology, UnitSpec};
use crate::health::run_health;
use crate::router::{IntentRouter, RoutingDecision};
use crate::store::RunStore;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_agent_loop::{
    AgentError, AgentEvent, AgentHandle, AgentLoop, AgentRunResult, BashTool, ObservedEvent,
    ReadFileTool, WriteFileTool,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub struct ScheduledRun {
    pub run_id: String,
    pub results: Vec<(String, AgentRunResult)>,
    pub routing_decision: Option<RoutingDecision>,
}

pub struct Scheduler {
    config: OrchestraConfig,
    store: RunStore,
    router: IntentRouter,
}

impl Scheduler {
    pub fn new(config: OrchestraConfig) -> Self {
        let store = RunStore::new(config.store_root.clone());
        let router = IntentRouter::new(config.base.clone());
        Self { config, store, router }
    }

    pub fn store(&self) -> &RunStore {
        &self.store
    }

    pub fn router(&self) -> &IntentRouter {
        &self.router
    }

    /// Run the cheap health/diagnostics checks (writable store + scratch, and a
    /// reachable or mocked LLM). B stays a pure scheduler: `health` never drives
    /// any unit's turns.
    pub async fn health(&self, use_mock: bool) -> crate::health::HealthReport {
        run_health(&self.config, use_mock).await
    }

    pub fn spawn_unit(&self, spec: &UnitSpec, use_mock: bool) -> AgentLoop {
        let cfg = spec
            .config
            .clone()
            .with_scratchpad_dir(self.config.scratch_root.join(&spec.id));

        let mut agent = AgentLoop::new(cfg).with_id(&spec.id);
        if use_mock {
            agent = agent.with_custom_client(Arc::new(crate::mock::RoleMockClient::new(&spec.role)));
        }
        if spec.register_builtins {
            agent.register_tool(Arc::new(BashTool::default()));
            agent.register_tool(Arc::new(ReadFileTool::default()));
            agent.register_tool(Arc::new(WriteFileTool::default()));
        }
        agent
    }

    /// Dispatch every unit according to the configured topology.
    pub async fn dispatch(
        &self,
        prompt: impl Into<String>,
        use_mock: bool,
        cancel: Option<CancellationToken>,
    ) -> Result<ScheduledRun, AgentError> {
        self.dispatch_with_events(prompt, use_mock, cancel, None).await
    }

    /// Dispatch every unit and optionally stream live ObservedEvents to an external channel (e.g. TUI).
    pub async fn dispatch_with_events(
        &self,
        prompt: impl Into<String>,
        use_mock: bool,
        cancel: Option<CancellationToken>,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
    ) -> Result<ScheduledRun, AgentError> {
        let prompt = prompt.into();
        let run_id = new_run_id();
        let cancel = cancel.unwrap_or_default();

        let (effective_topology, routing_decision) = if self.config.topology == Topology::Auto {
            let decision = self.router.route(&prompt, use_mock).await;
            info!(
                run_id = %run_id,
                selected = ?decision.topology,
                reason = %decision.reason,
                "Auto routing completed"
            );
            (decision.topology, Some(decision))
        } else {
            (self.config.topology, None)
        };

        info!(run_id = %run_id, topology = ?effective_topology, units = self.config.units.len(), "orchestra dispatch");

        let results = match effective_topology {
            Topology::Parallel => self.run_parallel(&run_id, &prompt, use_mock, cancel, event_tx).await?,
            Topology::Sequential => self.run_sequential(&run_id, &prompt, use_mock, cancel, event_tx).await?,
            Topology::Single | Topology::Auto => self.run_single(&run_id, &prompt, use_mock, cancel, event_tx).await?,
        };

        Ok(ScheduledRun {
            run_id,
            results,
            routing_decision,
        })
    }

    async fn run_single(
        &self,
        run_id: &str,
        prompt: &str,
        use_mock: bool,
        cancel: CancellationToken,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
    ) -> Result<Vec<(String, AgentRunResult)>, AgentError> {
        let spec = if let Some(first) = self.config.units.first() {
            first.clone()
        } else {
            let base = self
                .config
                .base
                .clone()
                .unwrap_or_else(|| thunder_agent_loop::AgentConfig::new("gpt-4o"));
            UnitSpec::new("agent", "assistant", base).with_builtins()
        };

        let agent = self.spawn_unit(&spec, use_mock);
        let mut handle = agent.start(prompt.to_string(), Some(cancel))?;
        spawn_event_forwarder(&mut handle, event_tx);
        let result = handle.join().await?;
        persist(self.store(), run_id, &spec.role, &result).await;

        Ok(vec![(spec.role, result)])
    }

    async fn run_parallel(
        &self,
        run_id: &str,
        prompt: &str,
        use_mock: bool,
        cancel: CancellationToken,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
    ) -> Result<Vec<(String, AgentRunResult)>, AgentError> {
        let mut started: Vec<(String, AgentHandle)> = Vec::new();

        for spec in &self.config.units {
            let agent = self.spawn_unit(spec, use_mock);
            let mut handle = agent.start(prompt.to_string(), Some(cancel.clone()))?;
            spawn_event_forwarder(&mut handle, event_tx.clone());
            started.push((spec.role.clone(), handle));
        }

        let mut out = Vec::with_capacity(started.len());
        for (role, handle) in started {
            let result = handle.join().await?;
            persist(self.store(), run_id, &role, &result).await;
            out.push((role, result));
        }
        Ok(out)
    }

    async fn run_sequential(
        &self,
        run_id: &str,
        prompt: &str,
        use_mock: bool,
        cancel: CancellationToken,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
    ) -> Result<Vec<(String, AgentRunResult)>, AgentError> {
        let mut out = Vec::new();
        let mut incoming = prompt.to_string();

        for spec in &self.config.units {
            if cancel.is_cancelled() {
                break;
            }
            let task = format!(
                "You are the `{}` unit.\nIncoming brief:\n{}\n\nDo your part and stop when done.",
                spec.role, incoming
            );
            let agent = self.spawn_unit(spec, use_mock);
            let mut handle = agent.start(task, Some(cancel.clone()))?;
            spawn_event_forwarder(&mut handle, event_tx.clone());
            let result = handle.join().await?;
            persist(self.store(), run_id, &spec.role, &result).await;
            incoming = result
                .final_content
                .clone()
                .unwrap_or_else(|| format!("({} finished with no content)", spec.id));
            out.push((spec.role.clone(), result));
        }

        Ok(out)
    }
}

fn spawn_event_forwarder(
    handle: &mut AgentHandle,
    custom_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
) {
    if let Some(mut rx) = handle.take_events() {
        tokio::spawn(async move {
            while let Some(observed) = rx.recv().await {
                if let Some(tx) = &custom_tx {
                    let _ = tx.send(observed);
                } else {
                    match observed.event {
                        AgentEvent::TurnStart { turn, .. } => {
                            println!("▶ [{}] turn {turn}", observed.agent_id);
                        }
                        AgentEvent::TokenDelta { delta, .. } => {
                            print!("{delta}");
                        }
                        AgentEvent::ToolExecResult { name, result, .. } => {
                            println!(
                                "\n⚙️  [{}] {name} ({}ms)",
                                observed.agent_id, result.duration_ms
                            );
                        }
                        AgentEvent::TurnEnd {
                            finish_reason,
                            stats,
                            ..
                        } => {
                            println!(
                                "\n⏹ [{}] turn {} ({finish_reason}, {}ms)",
                                observed.agent_id, stats.turn, stats.duration_ms
                            );
                        }
                        AgentEvent::Error { message, .. } => {
                            eprintln!("❌ [{}] {message}", observed.agent_id);
                        }
                        _ => {}
                    }
                }
            }
        });
    }
}

async fn persist(store: &RunStore, run_id: &str, role: &str, result: &AgentRunResult) {
    match store.save(run_id, role, result).await {
        Ok(path) => info!(agent_id = %result.agent_id, path = %path.display(), "persisted unit result"),
        Err(err) => error!(agent_id = %result.agent_id, error = %err, "failed to persist unit result"),
    }
}

fn new_run_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("run_{}_{ms}", std::process::id())
}
