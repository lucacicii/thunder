//! Live checks against DeepSeek's Search/Fetch API.
//!
//! Everything here is `#[ignore]`d: it needs a real credential and spends real
//! money (5 CNY per 1000 successful requests). Run it when the key or the
//! endpoint changes:
//!
//! ```text
//! DEEPSEEK_API_KEY=sk-… cargo test -p thunder-agent-root \
//!     --test web_deepseek_live_test -- --ignored --nocapture
//! ```
//!
//! The key is resolved through the same precedence the plugin uses, so a
//! `deepseek` entry in `~/.thunder/auth.json` works without exporting anything.

use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_providers::auth::AuthFile;
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_agent_root::plugins::web::deepseek::{self, DeepSeekClient};
use thunder_agent_root::plugins::web::fetch::WebFetchTool;
use thunder_agent_root::plugins::web::{FetchCache, WebSearchTool};
use thunder_agent_root::thunder_config::{WebFetchBackend, WebSearchBackend};

fn run_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "c1".to_string(),
        turn: 1,
        ..Default::default()
    }
}

/// The credential the plugin would resolve, or a skip.
async fn client() -> Option<DeepSeekClient> {
    let auth = AuthFile::load_default().await;
    match deepseek::resolve(None, &ProviderRegistry::default(), &auth) {
        Some(config) => Some(DeepSeekClient::new(config)),
        None => {
            eprintln!("skipping: no DeepSeek credential resolves (auth.json / env)");
            None
        }
    }
}

#[tokio::test]
#[ignore = "spends a real DeepSeek request"]
async fn search_returns_summaries_and_dates() {
    let Some(client) = client().await else { return };
    let tool = WebSearchTool::new()
        .with_deepseek(Some(client))
        .with_backend(WebSearchBackend::DeepSeek)
        .with_ori_question(Some("How does tokio schedule tasks?".to_string()));

    let out = tool
        .execute(
            serde_json::json!({"query": "tokio work stealing scheduler", "limit": 5}),
            &run_ctx(),
        )
        .await
        .expect("the search endpoint answers");
    println!("{out}");

    assert!(out.contains("via DeepSeek"), "{out}");
    assert!(out.contains("[1] "), "no ranked results: {out}");
    assert!(out.contains("https://"), "{out}");
    // The long model-facing summary is the whole point of this backend; a
    // reply that only carries snippets means the field stopped being returned.
    assert!(
        out.split("https://")
            .nth(1)
            .map(|s| s.len() > 200)
            .unwrap_or(false),
        "results look like short snippets, not summaries: {out}"
    );
}

#[tokio::test]
#[ignore = "spends a real DeepSeek request"]
async fn fetch_returns_markdown() {
    let Some(client) = client().await else { return };
    let tool = WebFetchTool::new(
        ProviderRegistry::default(),
        Arc::new(FetchCache::new(Duration::from_secs(60), 4)),
    )
    .with_deepseek(Some(client))
    .with_backend(WebFetchBackend::DeepSeek);

    let out = tool
        .execute(
            serde_json::json!({"url": "https://example.com", "prompt": "What is this page for?"}),
            &run_ctx(),
        )
        .await
        .expect("the fetch endpoint answers");
    println!("{out}");

    assert!(out.contains("documentation examples"), "{out}");
    assert!(!out.contains("<html"), "markup reached the model: {out}");
}
