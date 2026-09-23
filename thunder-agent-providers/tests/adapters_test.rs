use serde_json::json;
use std::collections::HashMap;
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_loop::types::tool::ToolDefinition;
use thunder_agent_providers::adapters::resolve_adapter;
use thunder_agent_providers::prelude::*;

fn make_dummy_spec(provider: &str, id: &str, api: ProviderApi) -> ModelSpec {
    ModelSpec {
        provider: provider.to_string(),
        id: id.to_string(),
        name: id.to_string(),
        api,
        base_url: "https://api.example.com".to_string(),
        api_key: Some("test-key".to_string()),
        headers: HashMap::new(),
        reasoning: false,
        context_window: 128_000,
        max_tokens: 4096,
        available: true,
        supports_developer_role: false,
        supports_reasoning_effort: false,
        max_tokens_field: "max_tokens".to_string(),
        thinking_levels: vec!["off".to_string()],
        default_thinking_level: "off".to_string(),
    }
}

#[test]
fn test_provider_api_from_str_loose() {
    assert_eq!(
        ProviderApi::from_str_loose("claude"),
        Some(ProviderApi::AnthropicMessages)
    );
    assert_eq!(
        ProviderApi::from_str_loose("anthropic"),
        Some(ProviderApi::AnthropicMessages)
    );
    assert_eq!(
        ProviderApi::from_str_loose("gemini"),
        Some(ProviderApi::GoogleGenerateContent)
    );
    assert_eq!(
        ProviderApi::from_str_loose("google"),
        Some(ProviderApi::GoogleGenerateContent)
    );
    assert_eq!(
        ProviderApi::from_str_loose("ollama"),
        Some(ProviderApi::Ollama)
    );
    assert_eq!(
        ProviderApi::from_str_loose("chat-completions"),
        Some(ProviderApi::OpenAiCompletions)
    );
}

#[test]
fn test_deepseek_v4_1_flash_official_spec_adapter() {
    let mut spec = make_dummy_spec("deepseek", "deepseek-flash", ProviderApi::OpenAiCompletions);
    spec.reasoning = true;
    let adapter = resolve_adapter(&spec);
    assert_eq!(adapter.name(), "deepseek-v4");

    let tc = ToolCall::new_function("call_bash_1", "bash", "{\"command\":\"cargo test\"}");
    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![
            ChatMessage::user("Please run tests"),
            ChatMessage::assistant(None, Some(vec![tc])),
            ChatMessage::tool("call_bash_1", "test result: ok. 15 passed", None),
        ],
        tools: vec![],
        model: None,
        temperature: Some(0.6),
        top_p: Some(0.95),
        max_tokens: Some(8192),
        thinking_level: None,
    };

    let payload = adapter.adapt_payload(&spec, &options);
    assert_eq!(payload["model"], "deepseek-flash");
    // DeepSeek Thinking Mode
    assert_eq!(payload["thinking"]["type"], "enabled");
    assert_eq!(payload["reasoning_effort"], "high");
    // Assistant message must maintain structure for multi-turn tool call compliance
    assert_eq!(payload["messages"].as_array().unwrap().len(), 3);
    assert_eq!(payload["messages"][1]["role"], "assistant");
    assert!(payload["messages"][1]["tool_calls"].is_array());
}

#[test]
fn test_openai_reasoning_adapter_contract() {
    let spec = make_dummy_spec("openai", "o3-mini", ProviderApi::OpenAiCompletions);
    let adapter = resolve_adapter(&spec);
    assert_eq!(adapter.name(), "openai-reasoning");

    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![
            ChatMessage::system("system prompt text"),
            ChatMessage::user("run task"),
        ],
        tools: vec![],
        model: None,
        temperature: Some(0.8), // must be dropped
        top_p: Some(0.95),      // must be dropped
        max_tokens: Some(3000),
        thinking_level: None,
    };

    let payload = adapter.adapt_payload(&spec, &options);
    assert_eq!(payload["model"], "o3-mini");
    assert!(payload.get("temperature").is_none());
    assert!(payload.get("top_p").is_none());
    assert_eq!(payload["max_completion_tokens"], 3000);
    assert_eq!(payload["messages"][0]["role"], "developer");
    assert_eq!(payload["messages"][0]["content"], "system prompt text");
}

#[test]
fn test_anthropic_adapter_contract() {
    let spec = make_dummy_spec("anthropic", "claude-3-7-sonnet", ProviderApi::AnthropicMessages);
    let adapter = resolve_adapter(&spec);
    assert_eq!(adapter.name(), "anthropic-messages");

    let tool = ToolDefinition::new_function("test_tool", "A test tool", json!({"type": "object"}));
    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![
            ChatMessage::system("Anthropic system instructions"),
            ChatMessage::user("User question"),
        ],
        tools: vec![tool],
        model: None,
        temperature: Some(0.5),
        top_p: None,
        max_tokens: Some(4096),
        thinking_level: None,
    };

    let payload = adapter.adapt_payload(&spec, &options);
    // Anthropic separates system into top-level property
    assert_eq!(payload["system"], "Anthropic system instructions");
    // Messages only contain non-system messages
    assert_eq!(payload["messages"].as_array().unwrap().len(), 1);
    assert_eq!(payload["messages"][0]["role"], "user");
    // Tools are mapped to Anthropic format (input_schema)
    assert_eq!(payload["tools"][0]["name"], "test_tool");
    assert!(payload["tools"][0]["input_schema"].is_object());
}

#[test]
fn test_google_adapter_contract() {
    let spec = make_dummy_spec("google", "gemini-1.5-pro", ProviderApi::GoogleGenerateContent);
    let adapter = resolve_adapter(&spec);
    assert_eq!(adapter.name(), "google-gemini");

    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![
            ChatMessage::system("Google system prompt"),
            ChatMessage::user("Search query"),
        ],
        tools: vec![],
        model: None,
        temperature: Some(0.2),
        top_p: None,
        max_tokens: Some(8192),
        thinking_level: None,
    };

    let payload = adapter.adapt_payload(&spec, &options);
    // Google uses systemInstruction
    assert_eq!(
        payload["systemInstruction"]["parts"][0]["text"],
        "Google system prompt"
    );
    // Contents array uses parts
    assert_eq!(payload["contents"][0]["role"], "user");
    assert_eq!(payload["contents"][0]["parts"][0]["text"], "Search query");
    assert!((payload["generationConfig"]["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-4);
}

#[test]
fn test_ollama_adapter_contract() {
    let spec = make_dummy_spec("ollama", "llama3.2", ProviderApi::Ollama);
    let adapter = resolve_adapter(&spec);
    assert_eq!(adapter.name(), "ollama-native");

    let options = thunder_agent_loop::stream::client::ChatRequestOptions {
        messages: vec![ChatMessage::user("Hi Ollama")],
        tools: vec![],
        model: None,
        temperature: Some(0.6),
        top_p: None,
        max_tokens: Some(512),
        thinking_level: None,
    };

    let payload = adapter.adapt_payload(&spec, &options);
    assert_eq!(payload["model"], "llama3.2");
    assert!((payload["options"]["temperature"].as_f64().unwrap() - 0.6).abs() < 1e-4);
    assert_eq!(payload["options"]["num_predict"], 512);
}
