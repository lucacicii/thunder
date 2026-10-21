//! Coverage for the `web_fetch` tool contributed by the web plugin.
//!
//! The non-network cases take no dependencies at all: argument validation and
//! URL policy run before a socket is opened, so they hold in CI and offline.
//! The two `#[ignore]`d cases are the real end-to-end checks — one against
//! `example.com` with no utility model (markdown fallback), one with the
//! machine's configured utility model (summarisation). Run them with
//! `cargo test -p thunder-agent-root --test web_fetch_tool_test -- --ignored`.

use std::sync::Arc;
use std::time::Duration;
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_agent_root::plugins::web::fetch::WebFetchTool;
use thunder_agent_root::plugins::web::{FetchCache, WEB_FETCH_TOOL};

/// A tool with an empty registry (no utility model) and an isolated cache.
fn offline_tool() -> WebFetchTool {
    WebFetchTool::new(
        ProviderRegistry::default(),
        Arc::new(FetchCache::new(Duration::from_secs(60), 4)),
    )
}

fn run_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "c1".to_string(),
        turn: 1,
        ..Default::default()
    }
}

#[test]
fn tool_definition_and_effect() {
    let tool = offline_tool();
    let definition = tool.definition();
    assert_eq!(definition.function.name, WEB_FETCH_TOOL);

    // A page fetch is a read: plan mode must not have to approve looking
    // something up, and no approval mode should nag about it.
    assert_eq!(tool.effect(), ToolEffect::Read);

    let required = definition.function.parameters["required"]
        .as_array()
        .expect("required should be an array");
    let required: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(required, vec!["url", "prompt"]);
}

#[tokio::test]
async fn missing_arguments_error() {
    let tool = offline_tool();

    let err = tool
        .execute(serde_json::json!({}), &run_ctx())
        .await
        .unwrap_err();
    assert_eq!(err, "Missing 'url' parameter");

    let err = tool
        .execute(
            serde_json::json!({"url": "https://example.com"}),
            &run_ctx(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, "Missing 'prompt' parameter");
}

#[tokio::test]
async fn invalid_url_errors_before_any_network() {
    let tool = offline_tool();
    let err = tool
        .execute(
            serde_json::json!({"url": "http://localhost:9", "prompt": "x"}),
            &run_ctx(),
        )
        .await
        .unwrap_err();
    assert!(err.contains("hostnames without a dot"), "{err}");
}

/// The live page's body text, verbatim. example.com is a fixed, tiny document,
/// which is what makes it usable as an end-to-end target at all; when the site
/// rewrites it, this constant and the assertions below are what to update.
const EXAMPLE_COM_BODY: &str = "This domain is for use in documentation examples without needing permission. This is not a service; avoid relying on it for testing and monitoring purposes.";

/// Network smoke: an empty registry has no utility model, so the answer is the
/// converted markdown itself.
#[tokio::test]
#[ignore = "network access required"]
async fn fetches_example_com_markdown_fallback() {
    let tool = offline_tool();
    let out = tool
        .execute(
            serde_json::json!({"url": "https://example.com", "prompt": "What is this page for?"}),
            &run_ctx(),
        )
        .await
        .expect("fetching example.com should succeed");
    println!("markdown fallback answer:\n{out}");
    assert!(
        out.contains(EXAMPLE_COM_BODY),
        "markdown fallback lost the page body: {out}"
    );
    assert!(!out.contains('<'), "markup reached the model: {out}");
}

/// Network smoke: with the machine's utility model configured, the answer is a
/// model summary rather than the page body.
#[tokio::test]
#[ignore = "network access and a configured utility model required"]
async fn summarizes_example_com_with_utility_model() {
    let registry = ProviderRegistry::load_default()
        .await
        .expect("models.json should load");
    let tool = WebFetchTool::new(
        registry,
        Arc::new(FetchCache::new(Duration::from_secs(60), 4)),
    );

    let out = tool
        .execute(
            serde_json::json!({"url": "https://example.com", "prompt": "What is this page for?"}),
            &run_ctx(),
        )
        .await
        .expect("fetching example.com should succeed");

    println!("utility-model answer:\n{out}");
    assert!(!out.trim().is_empty(), "empty answer");
    assert!(
        !out.contains("<web-page-content"),
        "the summarisation prompt echoed back, so the model branch was not taken: {out}"
    );
    // The fallback path returns the page verbatim; no summary of a two-sentence
    // page is that exact string. This discriminator does not depend on the
    // model's wording, unlike a "mentions X" assertion.
    assert_ne!(
        out.trim(),
        EXAMPLE_COM_BODY,
        "answer is the raw page, not a summary: {out}"
    );
}
