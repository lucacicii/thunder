use std::env;
use std::path::PathBuf;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_conversation::prelude::FsConversationStore;
use thunder_tui::prelude::*;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // The offline mock mode is gone: reject the flag explicitly instead of
    // silently ignoring it (an operator would otherwise believe they were
    // running against a fake model when a real one is being used).
    if args.iter().any(|a| a == "--mock") {
        eprintln!("✖ `--mock` has been removed: the TUI no longer ships a mock mode.");
        eprintln!(
            "  Configure a provider in ~/.thunder/models.json + auth.json and use a real model."
        );
        std::process::exit(2);
    }

    // Before anything else can log. Without this the kernel's own `info!` and
    // `warn!` go nowhere, which is why a TUI failure used to leave no evidence
    // at all.
    let log_path = init_tracing();
    tracing::info!(
        log = %log_path.display(),
        version = env!("CARGO_PKG_VERSION"),
        cwd = %env::current_dir().map(|d| d.display().to_string()).unwrap_or_default(),
        "Thunder TUI starting"
    );

    let registry = ProviderRegistry::load_default().await.unwrap_or_default();
    let available = registry.list_available();

    // No silent mock fallback: without a usable model the only honest move is
    // to say so and exit.
    let Some(model) = env::var("MODEL")
        .ok()
        .or_else(|| available.first().map(|m| m.selection_id()))
    else {
        eprintln!("✖ No LLM model available.");
        eprintln!();
        eprintln!("  Configure at least one provider in ~/.thunder/models.json + auth.json,");
        eprintln!("  or select an explicit model with MODEL=<provider/model-id>.");
        std::process::exit(1);
    };

    if !available.iter().any(|m| m.available) {
        eprintln!("⚠  No provider credentials found (~/.thunder/auth.json).");
        eprintln!(
            "   Runs will fail until a provider is configured; use /model to switch afterwards."
        );
    }

    let store_root = FsConversationStore::default_store_root();
    let store = FsConversationStore::new(store_root).await?;

    let mut app = App::new(model)
        .with_store(store)
        .with_provider_registry(registry);

    // Workspace root: `--workspace <path>` beats `THUNDER_WORKSPACE`, which
    // beats the current directory.
    match resolve_workspace_override(&args, env::var("THUNDER_WORKSPACE").ok().as_deref()) {
        Ok(Some(dir)) => app.workspace_dir = dir,
        Ok(None) => {}
        Err(msg) => {
            eprintln!("✖ {msg}");
            std::process::exit(2);
        }
    }

    // Persist initial session so it is immediately registered in the sidebar
    app.save_current_conversation().await;
    // Make the working area visible: it is the cwd unless overridden, and a
    // path outside it (or a shell command) is what the approval dialog asks
    // about.
    app.set_status_message(format!("Workspace: {}", app.workspace_dir.display()));

    // Optional specific session arg: --session <id>
    if let Some(pos) = args.iter().position(|a| a == "--session") {
        if let Some(session_id) = args.get(pos + 1) {
            app.load_conversation(session_id).await;
        }
    }

    let runner = TuiRunner::new(50);
    runner.run(app).await?;

    Ok(())
}

/// Send logs to a file, and return where.
///
/// Not to stdout: the runner owns the alternate screen, and a log line written
/// there tears the layout apart. Not to nothing either — that was the state
/// before, and it made every failure in a shipped TUI unreportable.
///
/// Default level is `warn`: a terminal session does not want the kernel's
/// per-turn chatter on disk, but a warning or an error should leave a trace.
/// `RUST_LOG` overrides it for a debugging session.
fn init_tracing() -> PathBuf {
    let dir = thunder_agent_loop::core::paths::thunder_config_dir().join("logs");
    let path = dir.join("tui.log");
    let _ = std::fs::create_dir_all(&dir);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    // A session that cannot open its log file still has to run: the panel is
    // the product, the log is only the evidence.
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
    }
    path
}

/// Resolve an explicit workspace override.
///
/// `Ok(None)` means "no override, keep the current directory". An override that
/// is not an existing directory is an error: silently falling back to the cwd
/// would put the jail somewhere the operator did not choose.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The log lives under the data root, not in the workspace: a trace of the
    /// terminal must not appear in the user's project.
    #[test]
    fn the_log_is_named_and_placed_predictably() {
        let dir = thunder_agent_loop::core::paths::thunder_config_dir().join("logs");
        assert!(dir.join("tui.log").starts_with(&dir));
        assert!(dir.ends_with("logs"));
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_override_keeps_cwd() {
        assert_eq!(resolve_workspace_override(&argv(&["tui"]), None), Ok(None));
        assert_eq!(
            resolve_workspace_override(&argv(&["tui"]), Some("  ")),
            Ok(None)
        );
    }

    #[test]
    fn flag_beats_env() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let got = resolve_workspace_override(
            &argv(&["tui", "--workspace", a.path().to_str().unwrap()]),
            Some(b.path().to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(got, Some(a.path().canonicalize().unwrap()));
    }

    #[test]
    fn env_used_when_no_flag() {
        let b = tempfile::tempdir().unwrap();
        let got =
            resolve_workspace_override(&argv(&["tui"]), Some(b.path().to_str().unwrap())).unwrap();
        assert_eq!(got, Some(b.path().canonicalize().unwrap()));
    }

    #[test]
    fn missing_dir_or_value_is_an_error() {
        assert!(resolve_workspace_override(&argv(&["tui", "--workspace"]), None).is_err());
        assert!(
            resolve_workspace_override(&argv(&["tui", "--workspace", "--session"]), None).is_err()
        );
        assert!(resolve_workspace_override(
            &argv(&["tui", "--workspace", "/no/such/dir/xyz"]),
            None
        )
        .is_err());
    }
}
