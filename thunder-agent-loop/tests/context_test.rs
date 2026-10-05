use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::core::token_estimator::estimate_token_count;
use thunder_agent_loop::types::config::{AgentConfig, DEFAULT_AUTONOMOUS_SYSTEM_PROMPT};
use thunder_agent_loop::types::message::ChatMessage;

#[test]
fn test_default_autonomous_system_prompt_content() {
    let cfg = AgentConfig::default();
    let prompt = cfg
        .system_prompt
        .as_deref()
        .expect("default system prompt must be set");
    assert!(prompt.starts_with("# Role & Philosophy"));
    assert!(prompt.contains("<intent_analysis>"));
    assert!(prompt.contains("Ask (question / consultation)"));
    assert!(prompt.contains("Plan (planning / architecture)"));
    assert!(prompt.contains("Write / Edit (file or content operations)"));
    assert!(prompt.contains("## Runtime Guardrails"));
    assert_eq!(prompt, DEFAULT_AUTONOMOUS_SYSTEM_PROMPT);
}

#[test]
fn test_context_buffer_token_accounting() {
    let mut ctx = ContextBuffer::new();
    ctx.set_system_prompt("You are a helpful coding assistant.");

    assert_eq!(ctx.len(), 1);
    let initial_tokens = ctx.estimated_tokens();
    assert!(initial_tokens > 5);

    ctx.push(ChatMessage::user("What is the speed of light in vacuum?"));
    assert_eq!(ctx.len(), 2);
    assert!(ctx.estimated_tokens() > initial_tokens);

    let messages = ctx.get_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content_str(),
        Some("You are a helpful coding assistant.")
    );
}

#[test]
fn test_cjk_token_estimation() {
    let text = "你好，这是高性能 Rust Agent Loop。";
    let tokens = estimate_token_count(text);
    assert!((10..=30).contains(&tokens));
}
