use thunder_protocol::{ChatMessage, ToolCall};
use thunder_suggest::SuggestionEngine;

#[tokio::test]
async fn engine_generates_empty_conversation_fallbacks() {
    let engine = SuggestionEngine::new();
    let suggestions = engine.generate_suggestions(&[]).await;
    assert!(!suggestions.is_empty());
    assert!(suggestions[0].contains("架构"));
}

#[tokio::test]
async fn engine_suggests_unit_tests_after_code_mutation() {
    let msgs = vec![
        ChatMessage::user("Refactor server logic"),
        ChatMessage::assistant(
            Some("Refactored server.rs".to_string()),
            Some(vec![ToolCall::new_function(
                "c1",
                "core__write_file",
                r#"{"path":"src/server.rs","content":"fn serve() {}"}"#,
            )]),
        ),
    ];

    let engine = SuggestionEngine::new();
    let suggestions = engine.generate_suggestions(&msgs).await;

    assert!(suggestions.iter().any(|s| s.contains("src/server.rs")));
    assert!(suggestions
        .iter()
        .any(|s| s.contains("测试用例") || s.contains("单元测试")));
}

#[tokio::test]
async fn engine_suggests_execution_after_write_plan() {
    let msgs = vec![
        ChatMessage::user("Draft architecture plan"),
        ChatMessage::assistant(
            Some("Plan drafted".to_string()),
            Some(vec![ToolCall::new_function(
                "c2",
                "core__write_plan",
                r##"{"content":"# Plan"}"##,
            )]),
        ),
    ];

    let engine = SuggestionEngine::new();
    let suggestions = engine.generate_suggestions(&msgs).await;

    assert!(suggestions.iter().any(|s| s.contains("退出 Plan 模式")));
}
