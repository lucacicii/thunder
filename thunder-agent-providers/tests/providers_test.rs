use thunder_agent_providers::api::{ModelRef, ProviderApi};
use thunder_agent_providers::auth::AuthFile;
use thunder_agent_providers::catalog::ProviderRegistry;
use thunder_agent_providers::config::ModelsFile;

#[test]
fn parses_openai_chat_and_responses() {
    let raw = r#"{
      "providers": {
        "openai": {
          "baseUrl": "https://api.openai.com/v1",
          "api": "openai-completions",
          "apiKey": "sk-test",
          "models": [
            { "id": "gpt-4o" },
            { "id": "gpt-5", "api": "openai-responses" }
          ]
        }
      }
    }"#;
    let file = ModelsFile::parse_json(raw).unwrap();
    let registry = ProviderRegistry::from_parts(file, &AuthFile::default()).unwrap();
    let chat = registry.resolve("openai/gpt-4o").unwrap();
    assert_eq!(chat.api, ProviderApi::OpenAiCompletions);
    assert!(chat.available);
    let resp = registry.resolve("gpt-5").unwrap();
    assert_eq!(resp.api, ProviderApi::OpenAiResponses);
}

#[test]
fn model_without_key_is_unavailable() {
    let raw = r#"{
      "providers": {
        "custom": {
          "baseUrl": "https://example.invalid/v1",
          "api": "openai-completions",
          "models": [{ "id": "local-model" }]
        }
      }
    }"#;
    let file = ModelsFile::parse_json(raw).unwrap();
    let registry = ProviderRegistry::from_parts(file, &AuthFile::default()).unwrap();
    let model = registry.resolve("custom/local-model").unwrap();
    assert!(!model.available);
}

#[test]
fn model_ref_parses_provider_and_id() {
    let parsed = ModelRef::parse("openai/gpt-4o");
    assert_eq!(parsed.provider, "openai");
    assert_eq!(parsed.model, "gpt-4o");
}

#[test]
fn normalize_openai_base_examples() {
    use thunder_agent_providers::openai::normalize_openai_base;
    assert_eq!(normalize_openai_base("https://opencode.ai/zen/go/v1"), "https://opencode.ai/zen/go/v1");
    assert_eq!(normalize_openai_base("https://opencode.ai/zen/go/v1/"), "https://opencode.ai/zen/go/v1");
    assert_eq!(normalize_openai_base("https://opencode.ai/zen/go/v1/chat/completions"), "https://opencode.ai/zen/go/v1");
    assert_eq!(normalize_openai_base("https://api.openai.com/v1"), "https://api.openai.com/v1");
    assert_eq!(normalize_openai_base("https://api.openai.com"), "https://api.openai.com/v1");
}

#[test]
fn wire_model_is_bare_id_not_selection_id() {
    // Regression: `provider/model` selection ids must never reach the API.
    // The routed client forces the wire model to the bare spec id.
    use thunder_agent_providers::catalog::ModelSpec;
    let spec = ModelSpec {
        provider: "cc-switch-open-code-go".to_string(),
        id: "ox-alpha-free".to_string(),
        name: "ox-alpha-free".to_string(),
        api: ProviderApi::OpenAiCompletions,
        base_url: "https://example.invalid/v1".to_string(),
        api_key: Some("k".to_string()),
        headers: Default::default(),
        reasoning: false,
        context_window: 128_000,
        max_tokens: 16_384,
        available: true,
        supports_developer_role: false,
        supports_reasoning_effort: false,
        max_tokens_field: "max_tokens".to_string(),
    };
    assert_eq!(spec.selection_id(), "cc-switch-open-code-go/ox-alpha-free");
    assert_eq!(spec.id, "ox-alpha-free");
}
