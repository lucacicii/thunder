use std::env;
use thunder_agent_loop::{init_logger, AgentConfig};
use thunder_orchestra::{OrchestraConfig, Scheduler, Topology, UnitSpec};

fn live_config(model: String) -> AgentConfig {
    AgentConfig::new(model).with_unlimited_turns()
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
        let use_mock = args.iter().any(|a| a == "--mock");
        let store_root = env::current_dir()?.join("runs");
        let orchestra = OrchestraConfig::new(Topology::Parallel)
            .with_store_root(&store_root)
            .with_scratch_root(env::temp_dir().join("thunder-orchestra"))
            .with_base(base.clone());
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
            UnitSpec::new("planner", "planner", base.clone()).with_builtins(),
            UnitSpec::new("reviewer", "reviewer", base.clone()).with_builtins(),
        ],
        Topology::Sequential => vec![
            UnitSpec::new("planner", "planner", base.clone()).with_builtins(),
            UnitSpec::new("coder", "coder", base.clone()).with_builtins(),
        ],
        Topology::Single => vec![
            UnitSpec::new("agent", "assistant", base.clone()).with_builtins(),
        ],
        Topology::Auto => vec![
            UnitSpec::new("planner", "planner", base.clone()).with_builtins(),
            UnitSpec::new("coder", "coder", base.clone()).with_builtins(),
            UnitSpec::new("reviewer", "reviewer", base.clone()).with_builtins(),
        ],
    };

    let store_root = env::current_dir()?.join("runs");
    let mut orchestra = OrchestraConfig::new(topology)
        .with_store_root(&store_root)
        .with_base(base.clone());
    for unit in units {
        orchestra = orchestra.with_unit(unit);
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
