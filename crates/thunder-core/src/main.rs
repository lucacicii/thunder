use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thunder_core::{
    AgentLoopConfig, ContextPruningConfig, ProviderClientDriver, ReActAgentLoop, ThunderConfig,
};
use thunder_executor::{McpServerConfig, McpToolProvider};
use thunder_providers::prelude::{client_for_selection, ProviderRegistry};
use thunder_registry::ToolRegistry;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let workspace_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = ThunderConfig::load_from_workspace(&workspace_cwd).await;

    if args.first().map(String::as_str) == Some("--list-models") {
        match ProviderRegistry::load_default().await {
            Ok(reg) => {
                let models = reg.list_available();
                println!("Thunder Providers ({} models):", models.len());
                for m in models {
                    let status = if m.available { "ready" } else { "no-auth" };
                    println!(
                        "  - {} [{}] (ctx={}, api={:?})",
                        m.selection_id(),
                        status,
                        m.context_window,
                        m.api
                    );
                }
            }
            Err(e) => {
                eprintln!("Failed to load provider catalog: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let registry = ToolRegistry::new();

    // 1. Mount `thunder-std` Core MCP server if available in sibling binary directory.
    if let Some(std_bin) = locate_thunder_std_binary() {
        let std_cfg = McpServerConfig::new(std_bin).as_core();
        if let Ok(provider) = McpToolProvider::connect("thunder-std", &std_cfg).await {
            let _ = registry.register_provider(provider).await;
        }
    }

    // 2. Mount configured external MCP servers from `.thunder/config.toml`.
    for (name, mcp_cfg) in &config.mcp_servers {
        if let Ok(provider) = McpToolProvider::connect(name, mcp_cfg).await {
            let _ = registry.register_provider(provider).await;
        }
    }

    // 3. Start the `.thunder/tools/` hot-reload poller.
    let tools_dir = workspace_cwd.join(".thunder").join("tools");
    let poll = Duration::from_millis(config.sandbox.debounce_ms.max(50));
    let _watcher = thunder_registry::start_hot_reload_watcher_with_interval(
        registry.clone(),
        tools_dir,
        None,
        poll,
    )
    .await
    .ok();

    if args.is_empty() || args[0] == "--list-tools" {
        let snap = registry.current();
        println!("Thunder Microkernel (snapshot v{}):", snap.version);
        for desc in snap.descriptors.values() {
            println!("  - {} ({}): {}", desc.name, desc.uri, desc.description);
        }
        return;
    }

    let prompt = args.join(" ");
    let session_id = format!("sess-{}", std::process::id());

    let (spec, llm_client) =
        match client_for_selection(&config.agent.model, config.sandbox.timeout_ms).await {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!(
                    "Failed to resolve model '{}' from ~/.thunder/models.json: {err}",
                    config.agent.model
                );
                std::process::exit(1);
            }
        };

    let driver = Arc::new(
        ProviderClientDriver::new(Arc::clone(&llm_client))
            .with_model(spec.id.clone())
            .with_session_id(session_id.clone()),
    );

    let pruning = ContextPruningConfig {
        max_context_tokens: spec.context_window.min(config.agent.max_context_tokens),
        ..ContextPruningConfig::default()
    };

    let loop_cfg = AgentLoopConfig {
        session_id: session_id.clone(),
        workspace_cwd,
        system_prompt: "You are Thunder Agent, a microkernel coding assistant.".to_string(),
        model: spec.id.clone(),
        budget: config.agent.to_budget(),
        stall: thunder_core::StallConfig {
            mutation_free_turns: config.agent.stall_mutation_free_turns,
            no_novel_turns: config.agent.stall_no_novel_turns,
        },
        pruning,
        tool_timeout: Duration::from_millis(config.sandbox.timeout_ms),
        cache_warm_settings: spec.prompt_cache_warm_settings(None),
    };

    let scratchpad = thunder_executor::ScratchpadManager::new(
        &session_id,
        thunder_executor::ScratchpadConfig::default(),
    );

    let mut agent = ReActAgentLoop::new(loop_cfg, registry, driver)
        .with_summarizer_client(llm_client)
        .with_scratchpad(scratchpad);
    match agent.run(prompt).await {
        Ok(outcome) => {
            println!("{}", outcome.final_response);
            if outcome.stopped_early() {
                // Hitting a ceiling is not a failure: the summary on stdout is the
                // result, so exit 0 and report the reason on stderr.
                eprintln!(
                    "note: run stopped early ({} after {} turn(s)); rerun the same prompt to continue.",
                    outcome.wire_finish_reason(),
                    outcome.turns_executed
                );
            }
        }
        Err(err) => {
            eprintln!("Thunder Agent error: {err}");
            std::process::exit(1);
        }
    }
}

fn locate_thunder_std_binary() -> Option<PathBuf> {
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("thunder-std");
            if sibling.exists() {
                return Some(sibling);
            }
        }
    }
    None
}
