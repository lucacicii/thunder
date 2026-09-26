use std::env;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_conversation::prelude::FsConversationStore;
use thunder_tui::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

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

    let store_root = FsConversationStore::default_store_root();
    let store = FsConversationStore::new(store_root).await?;

    let mut app = App::new(model)
        .with_store(store)
        .with_provider_registry(registry);

    // Persist initial session so it is immediately registered in the sidebar
    app.save_current_conversation().await;

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
