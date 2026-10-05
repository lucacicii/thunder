//! Headless benchmark probe: run one Thunder task and print machine-readable stats.
//!
//! Usage:
//!   MODEL=<model-id> THINKING=<level> BENCH_SESSION=<id> \
//!     cargo run -p thunder-agent-root --example bench_probe -- "<prompt>"
//!
//! Prints one `BENCH_JSON {...}` line on stdout when the run finishes.
//! This is an additive benchmark harness; it does not change library behaviour.

use std::env;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use thunder_agent_loop::prelude::*;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_agent_root::prelude::*;

#[derive(Default)]
struct Collector {
    turns: Vec<serde_json::Value>,
    compactions: Vec<serde_json::Value>,
    tool_names: Vec<String>,
    errors: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let prompt = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "Reply with exactly: ok".to_string());

    let model = env::var("MODEL").unwrap_or_else(|_| "stealth/space-bunny-alpha".to_string());
    let thinking = env::var("THINKING").ok();
    let session_id = env::var("BENCH_SESSION").unwrap_or_else(|_| "bench".to_string());
    let cwd = env::current_dir()?;

    let mut base_cfg = AgentConfig::new(model.clone())
        .with_workspace_dir(cwd)
        .with_session_id(session_id.clone());
    if let Ok(text) = env::var("SYSTEM") {
        base_cfg = base_cfg.with_system_prompt(text);
    }
    if let Some(level) = thinking.clone() {
        base_cfg = base_cfg.with_thinking_level(level);
    }

    let registry = match ProviderRegistry::load_default().await {
        Ok(r) => r,
        Err(err) => {
            eprintln!("failed to load provider registry (~/.thunder): {err}");
            std::process::exit(2);
        }
    };

    let root = StandardHostBuilder::new(Arc::new(
        thunder_conversation::prelude::MemoryConversationStore::new(),
    ))
    .build(ThunderRoot::new(base_cfg).with_provider_registry(registry.clone()));

    if registry.resolve(&model).is_none() {
        eprintln!("model '{model}' not found in ~/.thunder/models.json + auth.json");
        std::process::exit(2);
    }

    let options = RootRunOptions {
        session_id: Some(session_id),
        custom_client: None,
        cancellation_token: None,
        forced_plugins: None,
        register_builtins: true,
        thinking_level: thinking,
        permission: thunder_agent_loop::types::config::Permission::default(),
        pause_gate: None,
        steer_queues: None,
        ui: None,
        policy: None,
        route: None,
    };

    let started = Instant::now();
    let mut handle = root.execute(prompt, options).await?;
    let active_plugins = handle.selection.active_plugin_ids.clone();

    let collector = Arc::new(Mutex::new(Collector::default()));
    let sink = Arc::clone(&collector);
    let events = handle.take_events();
    let drain = tokio::spawn(async move {
        if let Some(mut rx) = events {
            while let Some(observed) = rx.recv().await {
                match observed.event {
                    AgentEvent::TurnEnd {
                        turn,
                        finish_reason,
                        stats,
                    } => {
                        sink.lock().unwrap().turns.push(serde_json::json!({
                            "turn": turn,
                            "finish_reason": finish_reason,
                            "prompt_tokens": stats.prompt_tokens,
                            "completion_tokens": stats.completion_tokens,
                            "cached_tokens": stats.cached_tokens,
                            "cache_write_tokens": stats.cache_write_tokens,
                            "reasoning_tokens": stats.reasoning_tokens,
                            "duration_ms": stats.duration_ms,
                            "tool_calls": stats.tool_calls_count,
                        }));
                    }
                    AgentEvent::ToolExecResult { name, .. } => {
                        sink.lock().unwrap().tool_names.push(name);
                    }
                    AgentEvent::ContextCompacted {
                        tokens_before,
                        tokens_after,
                        ..
                    } => {
                        sink.lock().unwrap().compactions.push(serde_json::json!({
                            "tokens_before": tokens_before,
                            "tokens_after": tokens_after,
                        }));
                    }
                    AgentEvent::Error { message, .. } => {
                        sink.lock().unwrap().errors.push(message);
                    }
                    _ => {}
                }
            }
        }
    });

    let result = handle.join().await?;
    // The event channel closes when the unit settles, so this terminates.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), drain).await;

    let stats = result.run_result.stats;
    let wall_ms = started.elapsed().as_millis() as u64;
    let final_text = result.final_content.unwrap_or_default();
    let c = collector.lock().unwrap();

    println!(
        "BENCH_JSON {}",
        serde_json::json!({
            "agent": "thunder",
            "model": model,
            "wall_ms": wall_ms,
            "finish_reason": format!("{:?}", result.run_result.finish_reason),
            "turns": stats.total_turns,
            "tool_executions": stats.total_tool_executions,
            "prompt_tokens": stats.total_prompt_tokens,
            "completion_tokens": stats.total_completion_tokens,
            "cached_tokens": stats.total_cached_tokens,
            "reasoning_tokens": stats.total_reasoning_tokens,
            "duration_ms": stats.total_duration_ms,
            "active_plugins": active_plugins,
            "tool_names": c.tool_names,
            "turn_stats": c.turns,
            "compactions": c.compactions,
            "errors": c.errors,
            "final_text": final_text,
        })
    );

    Ok(())
}
