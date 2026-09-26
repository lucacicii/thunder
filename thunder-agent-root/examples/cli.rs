use std::env;
use thunder_agent_loop::prelude::*;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_agent_root::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let prompt = if args.len() > 1 && !args[1].starts_with("--") {
        args[1].clone()
    } else {
        "Review repository architecture and manage conversation session".to_string()
    };

    let model = env::var("MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
    let base_cfg = AgentConfig::new(model.clone()).with_unlimited_turns();

    // Composition root: one provider registry feeds the outer agent
    // (via ThunderRoot), so LIVE mode has a single source of transport truth.
    let registry = match ProviderRegistry::load_default().await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("⚠  Failed to load provider registry (~/.thunder): {err}");
            ProviderRegistry::default()
        }
    };

    // 1. Initialize ThunderRoot through the shared host assembler so the CLI
    //    exposes exactly the same baseline capability set as the TUI and daemon.
    let root = StandardHostBuilder::new(std::sync::Arc::new(
        thunder_conversation::prelude::MemoryConversationStore::new(),
    ))
    .build(ThunderRoot::new(base_cfg.clone()).with_provider_registry(registry.clone()));

    println!("============================================================");
    println!("⚡ Thunder-Root Microkernel Host CLI");
    println!("============================================================");
    println!("▶ User Prompt: {}", prompt);
    println!("▶ Mode: LIVE LLM");
    println!(
        "▶ Registered Plugins in Registry: {:?}",
        root.registry()
            .list_manifests()
            .iter()
            .map(|m| &m.id)
            .collect::<Vec<_>>()
    );
    println!("------------------------------------------------------------");

    if registry.resolve(&model).is_none() {
        eprintln!(
            "✖  MODEL '{}' was not found in the provider registry (~/.thunder/models.json + auth.json).\n\
                Configure a provider there, or run with MODEL=<configured id>.",
            model
        );
        std::process::exit(1);
    }

    let options = RootRunOptions {
        session_id: Some("cli_session_1".to_string()),
        custom_client: None,
        cancellation_token: None,
        forced_plugins: None,
        register_builtins: true,
        thinking_level: None,
        role: None,
        permission: thunder_agent_loop::types::config::Permission::default(),
        pause_gate: None,
    };

    let mut handle = root.execute(prompt, options).await?;
    println!("🔍 Dynamic Plugin Selection Result:");
    println!(
        "   Active Plugins : {:?}",
        handle.selection.active_plugin_ids
    );
    println!("   Decision Reason: {}", handle.selection.reason);
    println!("   Confidence     : {:.2}", handle.selection.confidence);
    println!("------------------------------------------------------------");

    if let Some(mut rx) = handle.take_events() {
        tokio::spawn(async move {
            while let Some(observed) = rx.recv().await {
                match observed.event {
                    AgentEvent::TokenDelta { delta, .. } => {
                        print!("{delta}");
                    }
                    AgentEvent::ToolExecStart { name, .. } => {
                        println!("\n⚙️  Tool Executing: {name}");
                    }
                    AgentEvent::ToolExecResult { name, result, .. } => {
                        println!("✔  Tool Completed: {name} ({}ms)", result.duration_ms);
                    }
                    _ => {}
                }
            }
        });
    }

    let result = handle.join().await?;
    println!("\n------------------------------------------------------------");
    println!(
        "🏁 Root Execution Finished (Status: {:?})",
        result.run_result.finish_reason
    );
    println!(
        "📊 Total Turns: {}, Tool Executions: {}",
        result.run_result.stats.total_turns, result.run_result.stats.total_tool_executions
    );
    if let Some(final_text) = result.final_content {
        println!("📝 Final Output:\n{}", final_text);
    }
    println!("============================================================");

    Ok(())
}
