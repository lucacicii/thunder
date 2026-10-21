//! Coverage for the `web_search` tool contributed by the web plugin.
//!
//! The non-network cases hold offline: argument validation runs before a socket
//! is opened. The `#[ignore]`d case is the real end-to-end check against the
//! free engines — run it with
//! `cargo test -p thunder-agent-root --test web_search_tool_test -- --ignored --nocapture`.

use thunder_agent_types::policy::ToolEffect;
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_root::plugins::web::{WebSearchTool, WEB_SEARCH_TOOL};

fn tool() -> WebSearchTool {
    WebSearchTool::new()
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
    let tool = tool();
    let definition = tool.definition();
    assert_eq!(definition.function.name, WEB_SEARCH_TOOL);

    // Searching reads nothing in the workspace: plan mode must not have to
    // approve it, and no approval mode should nag about it.
    assert_eq!(tool.effect(), ToolEffect::Read);

    let required = definition.function.parameters["required"]
        .as_array()
        .expect("required should be an array");
    let required: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(required, vec!["query"]);
}

#[tokio::test]
async fn missing_and_empty_query_error_before_any_network() {
    let tool = tool();

    let err = tool
        .execute(serde_json::json!({}), &run_ctx())
        .await
        .unwrap_err();
    assert_eq!(err, "Missing 'query' parameter");

    let err = tool
        .execute(serde_json::json!({"query": "   "}), &run_ctx())
        .await
        .unwrap_err();
    assert!(err.contains("empty"), "{err}");

    let too_long = "a".repeat(401);
    let err = tool
        .execute(serde_json::json!({"query": too_long}), &run_ctx())
        .await
        .unwrap_err();
    assert!(err.contains("maximum length"), "{err}");
}

/// Real network: both backends are third-party services, so this asserts the
/// shape of a successful search rather than a fixed hit list.
#[tokio::test]
#[ignore = "hits the live DuckDuckGo and Exa endpoints"]
async fn searches_the_web_end_to_end() {
    let tool = tool();
    let out = tool
        .execute(
            serde_json::json!({"query": "rust programming language", "limit": 5}),
            &run_ctx(),
        )
        .await
        .expect("at least one backend should answer");
    println!("{out}");

    assert!(out.contains("sources via "), "no backend label in: {out}");
    assert!(out.contains("[1] "), "no ranked results in: {out}");
    assert!(out.contains("https://"), "results should carry URLs: {out}");
    assert!(
        !out.contains("<html") && !out.contains("anomaly-modal"),
        "the bot-detection page leaked into the output: {out}"
    );
}
