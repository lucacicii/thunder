use crate::config::{OrchestraConfig, Topology, UnitSpec};
use crate::health::run_health;
use crate::router::{IntentRouter, RoutingDecision};
use crate::store::RunStore;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_agent_loop::{
    AgentError, AgentEvent, AgentHandle, AgentLoop, AgentRunResult, BashTool, LLMClientTrait,
    ObservedEvent, ReadFileTool, WriteFileTool,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

pub struct ScheduledRun {
    pub run_id: String,
    pub results: Vec<(String, AgentRunResult)>,
    pub routing_decision: Option<RoutingDecision>,
    pub synthesis: Option<String>,
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

    pub fn spawn_unit(&self, spec: &UnitSpec, use_mock: bool) -> Result<AgentLoop, AgentError> {
        let mut cfg = spec
            .config
            .clone()
            .with_scratchpad_dir(self.config.scratch_root.join(&spec.id));

        if let Some(sys) = &spec.system_prompt {
            cfg.system_prompt = Some(sys.clone());
        }

        let unit_model = cfg.model.clone();

        // Resolve the real-mode client BEFORE moving `cfg` into the agent.
        let resolved: Option<Arc<dyn LLMClientTrait>> = if use_mock {
            None
        } else if let Some(factory) = self.config.client_factory.as_ref() {
            match factory(&cfg) {
                Some(client) => Some(client),
                None => {
                    return Err(AgentError::Config(format!(
                        "client factory returned no client for unit `{}` (model `{unit_model}`). \
                         Verify the model resolves in your provider registry (models.json / auth.json).",
                        spec.id
                    )));
                }
            }
        } else {
            // Fail fast: a real-mode run without a factory would silently give
            // every unit the default `UnconfiguredLLMClient` and die mid-run.
            return Err(AgentError::Config(
                "real (non-mock) orchestra runs require a client factory: attach one via \
                 `OrchestraConfig::with_client_factory` (hosts typically build it from \
                 `ProviderRegistry` + `client_for`)"
                    .to_string(),
            ));
        };

        let mut agent = AgentLoop::new(cfg).with_id(&spec.id);
        if use_mock {
            agent = agent.with_custom_client(Arc::new(crate::mock::RoleMockClient::new(&spec.role)));
        } else if let Some(client) = resolved {
            agent = agent.with_custom_client(client);
        }
        if spec.register_builtins {
            agent.register_tool(Arc::new(BashTool::default()));
            agent.register_tool(Arc::new(ReadFileTool::default()));
            agent.register_tool(Arc::new(WriteFileTool::default()));
        }
        Ok(agent)
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

        // Fail fast before any unit starts: real mode without a client factory
        // is a host wiring bug, not a per-unit runtime failure.
        if !use_mock && self.config.client_factory.is_none() {
            return Err(AgentError::Config(
                "real (non-mock) orchestra runs require a client factory: attach one via \
                 `OrchestraConfig::with_client_factory` (hosts typically build it from \
                 `ProviderRegistry` + `client_for`)"
                    .to_string(),
            ));
        }

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
            Topology::FanOut => self.run_fan_out(&run_id, &prompt, use_mock, cancel, event_tx).await?,
            Topology::Sequential => self.run_sequential(&run_id, &prompt, use_mock, cancel, event_tx).await?,
            Topology::Single | Topology::Auto => self.run_single(&run_id, &prompt, use_mock, cancel, event_tx).await?,
        };

        let synthesis = if self.config.synthesize && results.len() > 1 {
            let synth_client = if use_mock {
                None
            } else {
                self.resolve_synthesizer_client()
            };
            match crate::synthesizer::Synthesizer::synthesize(
                &prompt,
                &results,
                self.config.synthesizer.as_ref(),
                synth_client,
                use_mock,
            )
            .await
            {
                Ok(s) => Some(s),
                Err(err) => {
                    tracing::warn!(error = %err, "Failed to synthesize orchestra results");
                    None
                }
            }
        } else {
            None
        };

        Ok(ScheduledRun {
            run_id,
            results,
            routing_decision,
            synthesis,
        })
    }

    /// Resolve the LLM client for the synthesis aggregator via the configured
    /// factory (prefers the dedicated synthesizer spec's config, falls back to
    /// `base`). Returns `None` when no factory is attached — callers must then
    /// degrade honestly instead of faking a synthesized report.
    fn resolve_synthesizer_client(&self) -> Option<Arc<dyn thunder_agent_loop::LLMClientTrait>> {
        let factory = self.config.client_factory.as_ref()?;
        let cfg = self
            .config
            .synthesizer
            .as_ref()
            .map(|s| s.config.clone())
            .or_else(|| self.config.base.clone())?;
        factory(&cfg)
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

        let agent = self.spawn_unit(&spec, use_mock)?;
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
            // Explicitly inject role context & responsibilities to ensure genuine multi-perspective execution
            let role_prompt = if let Some(sys) = &spec.system_prompt {
                format!(
                    "You are the `{}` unit.\nRole Directive:\n{}\n\nTask Brief:\n{}\n\nExecute your role's specific responsibilities and stop when done.",
                    spec.role, sys, prompt
                )
            } else {
                format!(
                    "You are the `{}` unit with specialized responsibility in this team.\nTask Brief:\n{}\n\nFocus strictly on your domain ({}) and provide your specialized analysis and output.",
                    spec.role, prompt, spec.role
                )
            };

            let agent = self.spawn_unit(spec, use_mock)?;
            let mut handle = agent.start(role_prompt, Some(cancel.clone()))?;
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

    async fn run_fan_out(
        &self,
        run_id: &str,
        prompt: &str,
        use_mock: bool,
        cancel: CancellationToken,
        event_tx: Option<tokio::sync::mpsc::UnboundedSender<ObservedEvent>>,
    ) -> Result<Vec<(String, AgentRunResult)>, AgentError> {
        use crate::decomposer::{HeuristicDecomposer, TaskDecomposer};
        let decomposer = HeuristicDecomposer;
        let subtasks = decomposer.decompose(prompt, &self.config.units);

        let mut started: Vec<(String, AgentHandle)> = Vec::new();

        for subtask in subtasks {
            let spec = self
                .config
                .units
                .iter()
                .find(|u| u.role == subtask.role)
                .cloned()
                .unwrap_or_else(|| {
                    if let Some(first) = self.config.units.first() {
                        first.clone()
                    } else {
                        let base = self
                            .config
                            .base
                            .clone()
                            .unwrap_or_else(|| thunder_agent_loop::AgentConfig::new("gpt-4o"));
                        UnitSpec::new(&subtask.id, &subtask.role, base).with_builtins()
                    }
                });

            let mut unit_spec = spec.clone();
            unit_spec.id = format!("{}_{}", spec.id, subtask.id);
            let agent = self.spawn_unit(&unit_spec, use_mock)?;
            let mut handle = agent.start(subtask.prompt, Some(cancel.clone()))?;
            spawn_event_forwarder(&mut handle, event_tx.clone());
            started.push((format!("{}: {}", subtask.role, subtask.title), handle));
        }

        let mut out = Vec::with_capacity(started.len());
        for (label, handle) in started {
            let result = handle.join().await?;
            persist(self.store(), run_id, &label, &result).await;
            out.push((label, result));
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
            let agent = self.spawn_unit(spec, use_mock)?;
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
