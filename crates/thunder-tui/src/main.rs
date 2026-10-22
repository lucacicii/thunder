use std::env;
use std::path::PathBuf;
use thunder_core::{FsConversationStore, ThunderConfig};
use thunder_executor::{McpServerConfig, McpToolProvider};
use thunder_providers::prelude::ProviderRegistry;
use thunder_registry::{start_hot_reload_watcher, ToolRegistry};
use thunder_tui::{App, TuiRunner};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|a| a == "--mock") {
        eprintln!("✖ `--mock` has been removed: configure a provider in ~/.thunder/models.json + auth.json.");
        std::process::exit(2);
    }

    let log_path = init_tracing();
    tracing::info!(
        log = %log_path.display(),
        version = env!("CARGO_PKG_VERSION"),
        "Thunder TUI starting"
    );

    let registry = ProviderRegistry::load_default().await.unwrap_or_default();
    let available = registry.list_available();

    let Some(model) = env::var("MODEL")
        .ok()
        .or_else(|| available.first().map(|m| m.selection_id()))
    else {
        eprintln!("✖ No LLM model available in ~/.thunder/models.json + auth.json.");
        std::process::exit(1);
    };

    let mut workspace_dir = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match resolve_workspace_override(&args, env::var("THUNDER_WORKSPACE").ok().as_deref()) {
        Ok(Some(dir)) => workspace_dir = dir,
        Ok(None) => {}
        Err(msg) => {
            eprintln!("✖ {msg}");
            std::process::exit(2);
        }
    }

    let tool_registry = ToolRegistry::new();
    if let Some(std_bin) = locate_thunder_std_binary() {
        let std_cfg = McpServerConfig::new(std_bin).as_core();
        if let Ok(provider) = McpToolProvider::connect("thunder-std", &std_cfg).await {
            let _ = tool_registry.register_provider(provider).await;
        }
    }

    let cfg = ThunderConfig::load_from_workspace(&workspace_dir).await;
    for (name, mcp_cfg) in &cfg.mcp_servers {
        if let Ok(provider) = McpToolProvider::connect(name, mcp_cfg).await {
            let _ = tool_registry.register_provider(provider).await;
        }
    }

    let tools_dir = workspace_dir.join(".thunder").join("tools");
    let _watcher = start_hot_reload_watcher(tool_registry.clone(), tools_dir, None)
        .await
        .ok();

    let store_root = FsConversationStore::default_store_root();
    let store = FsConversationStore::new(store_root).await?;

    let mem_bridge =
        thunder_locator::MemoryBridge::new(thunder_locator::MemoryBridge::default_db_path());
    let ws_reg = thunder_locator::WorkspaceRegistry::load_or_init(
        thunder_locator::WorkspaceRegistry::default_storage_path(),
        mem_bridge.clone(),
    )
    .await;
    let locator = std::sync::Arc::new(thunder_locator::WorkspaceLocator::new(ws_reg, mem_bridge));

    let mut app = App::new(model)
        .with_store(store)
        .with_provider_registry(registry)
        .with_tool_registry(tool_registry)
        .with_locator(locator);
    app.workspace_dir = workspace_dir;
    app.set_status_message(format!("Workspace: {}", app.workspace_dir.display()));

    if let Some(pos) = args.iter().position(|a| a == "--session") {
        if let Some(session_id) = args.get(pos + 1) {
            app.load_conversation(session_id).await;
        }
    }

    let runner = TuiRunner::new(250);
    runner.run(app).await?;
    Ok(())
}

fn init_tracing() -> PathBuf {
    let dir = thunder_protocol::thunder_config_dir().join("logs");
    let path = dir.join("tui.log");
    let _ = std::fs::create_dir_all(&dir);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
    }
    path
}

fn resolve_workspace_override(
    args: &[String],
    env_value: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let (raw, source) = match args.iter().position(|a| a == "--workspace") {
        Some(pos) => match args.get(pos + 1) {
            Some(v) if !v.starts_with("--") => (v.as_str(), "--workspace"),
            _ => return Err("`--workspace` requires a directory path.".to_string()),
        },
        None => match env_value.filter(|v| !v.trim().is_empty()) {
            Some(v) => (v, "THUNDER_WORKSPACE"),
            None => return Ok(None),
        },
    };
    let path = PathBuf::from(raw);
    path.canonicalize()
        .ok()
        .filter(|p| p.is_dir())
        .map(Some)
        .ok_or_else(|| format!("{source}: `{raw}` is not an existing directory."))
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
