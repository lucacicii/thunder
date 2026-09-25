use std::env;
use std::sync::Arc;
use thunder_agent_loop::{init_logger, AgentConfig};
use thunder_orchestra::{ClientFactory, OrchestraConfig, Scheduler, Topology, UnitSpec};

fn live_config(model: String) -> AgentConfig {
    AgentConfig::new(model).with_unlimited_turns()
}

/// Default role personas so CLI units carry genuinely different system prompts.
fn persona_for(role: &str) -> Option<&'static str> {
    match role {
        "planner" => Some(
            "You are the Planning Specialist. Decompose the brief into concrete, ordered steps, \
             identify risks and dependencies, and hand a precise actionable plan to the next unit. \
             Do not modify files yourself.",
        ),
        "coder" => Some(
            "You are the Implementation Specialist. Execute the incoming plan with precise, minimal \
             edits, verify your changes with the available tools, and report exactly what changed.",
        ),
        "reviewer" => Some(
            "You are the Review & Risk Specialist. Critique the work from a quality, correctness, and \
             security angle, list defects by severity, and propose concrete fixes.",
        ),
        _ => None,
    }
}

fn unit(id: &str, role: &str, base: &AgentConfig) -> UnitSpec {
    let mut spec = UnitSpec::new(id, role, base.clone()).with_builtins();
    if let Some(persona) = persona_for(role) {
        spec = spec.with_system_prompt(persona);
    }
    spec
}

/// Build the real-mode client factory from the default provider registry.
/// The factory resolves each unit's own `AgentConfig.model` so different units
/// may run different models.
async fn client_factory_or_none() -> Option<ClientFactory> {
    use thunder_agent_providers::prelude::{client_for, ProviderRegistry};
    let registry = ProviderRegistry::load_default().await.ok()?;
    Some(Arc::new(move |cfg: &AgentConfig| {
        registry
            .resolve(&cfg.model)
            .and_then(|spec| client_for(spec, cfg.request_timeout_ms).ok())
    }))
}

fn parse_args(args: &[String]) -> (Topology, String) {
    let mut topology = Topology::Auto;
    let mut prompt = "Review the current working directory and report findings.".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "auto" | "--auto" => topology = Topology::Auto,
            "parallel" | "--parallel" => topology = Topology::Parallel,
            "pipeline" | "sequential" | "--pipeline" => topology = Topology::Sequential,
            "single" | "--single" => topology = Topology::Single,
            flag if flag.starts_with("--") => {}
            text => {
                prompt = text.to_string();
                break;
            }
        }
        i += 1;
    }

    (topology, prompt)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logger();

    let args: Vec<String> = env::args().collect();
    let (topology, prompt) = parse_args(&args);
    let use_mock = args.iter().any(|a| a == "--mock");

    let model = env::var("MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
    let base = live_config(model);

    // `health` is orthogonal to topology
    if args.get(1).map(|s| s.as_str()) == Some("health")
        || args.iter().any(|a| a == "--health")
    {
        let store_root = env::current_dir()?.join("runs");
        let mut orchestra = OrchestraConfig::new(Topology::Parallel)
            .with_store_root(&store_root)
            .with_scratch_root(env::temp_dir().join("thunder-orchestra"))
            .with_base(base.clone());
        if !use_mock {
            if let Some(factory) = client_factory_or_none().await {
                orchestra = orchestra.with_client_factory(factory);
            }
        }
        let scheduler = Scheduler::new(orchestra);
        let report = scheduler.health(use_mock).await;
        println!("{}", serde_json::to_string_pretty(&report)?);
        std::process::exit(match report.status {
            thunder_orchestra::HealthStatus::Failed => 1,
            _ => 0,
        });
    }

    let units = match topology {
        Topology::Parallel => vec![
            unit("planner", "planner", &base),
            unit("reviewer", "reviewer", &base),
        ],
        Topology::Sequential => vec![
            unit("planner", "planner", &base),
            unit("coder", "coder", &base),
        ],
        Topology::Single => vec![
            UnitSpec::new("agent", "assistant", base.clone()).with_builtins(),
        ],
        Topology::FanOut => vec![
            UnitSpec::new("worker1", "worker", base.clone()).with_builtins(),
            UnitSpec::new("worker2", "worker", base.clone()).with_builtins(),
        ],
        Topology::Auto => vec![
            unit("planner", "planner", &base),
            unit("coder", "coder", &base),
            unit("reviewer", "reviewer", &base),
        ],
    };

    let store_root = env::current_dir()?.join("runs");
    let mut orchestra = OrchestraConfig::new(topology)
        .with_store_root(&store_root)
        .with_base(base.clone());
    if !use_mock {
        match client_factory_or_none().await {
            Some(factory) => {
                orchestra = orchestra.with_client_factory(factory);
            }
            None => {
                eprintln!(
                    "❌ real mode requested but no provider registry is available \
                     (check ~/.thunder/models.json + auth.json); rerun with --mock for a fixture run"
                );
                std::process::exit(2);
            }
        }
    }
    for unit_spec in units {
        orchestra = orchestra.with_unit(unit_spec);
    }

    println!(
        "{}  topology={topology:?}  units={}",
        if use_mock {
            "⚡ [Orchestra / Mock LLM]"
        } else {
            "⚡ [Orchestra / Live LLM]"
        },
        orchestra.units.len()
    );
    println!("⚡ Brief: {prompt}");
    println!("────────────────────────────────────────────────────────────");

    let scheduler = Scheduler::new(orchestra);
    let run = scheduler.dispatch(prompt, use_mock, None).await?;

    println!("────────────────────────────────────────────────────────────");
    println!("✨ run_id={}", run.run_id);
    if let Some(dec) = &run.routing_decision {
        println!("🧭 Autonomous Routing: {:?} (Confidence: {:.0}%)", dec.topology, dec.confidence * 100.0);
        println!("   Reason: {}", dec.reason);
    }
    for (role, result) in &run.results {
        println!(
            "   [{role}/{}] {:?}  turns={}  tools={}",
            result.agent_id,
            result.finish_reason,
            result.stats.total_turns,
            result.stats.total_tool_executions
        );
        if let Some(text) = &result.final_content {
            println!("      {text}");
        }
    }
    println!(
        "   stored under {}",
        store_root.join(&run.run_id).display()
    );

    Ok(())
}
