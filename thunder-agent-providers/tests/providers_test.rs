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
    assert_eq!(
        normalize_openai_base("https://opencode.ai/zen/go/v1"),
        "https://opencode.ai/zen/go/v1"
    );
    assert_eq!(
        normalize_openai_base("https://opencode.ai/zen/go/v1/"),
        "https://opencode.ai/zen/go/v1"
    );
    assert_eq!(
        normalize_openai_base("https://opencode.ai/zen/go/v1/chat/completions"),
        "https://opencode.ai/zen/go/v1"
    );
    assert_eq!(
        normalize_openai_base("https://api.openai.com/v1"),
        "https://api.openai.com/v1"
    );
    assert_eq!(
        normalize_openai_base("https://api.openai.com"),
        "https://api.openai.com/v1"
    );
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
        thinking_levels: vec!["off".to_string()],
        default_thinking_level: "off".to_string(),
        thinking_levels_probed: false,
        thinking_level_map: None,
        compat: None,
    };
    assert_eq!(spec.selection_id(), "cc-switch-open-code-go/ox-alpha-free");
    assert_eq!(spec.id, "ox-alpha-free");
}

#[tokio::test]
async fn test_thinking_levels_resolution_and_defaults() {
    use thunder_agent_providers::catalog::resolve_thinking_levels;

    // 1. Explicit configuration takes highest priority
    let explicit_levels = vec!["low".to_string(), "high".to_string()];
    let (levels, def) = resolve_thinking_levels(
        Some(&explicit_levels),
        Some("high"),
        false,
        false,
        "any-model",
    );
    assert_eq!(levels, vec!["low", "high"]);
    assert_eq!(def, "high");

    // 2. Known reasoning model by flag or id defaults to off/low/medium/high and medium/high
    let (r_levels, r_def) = resolve_thinking_levels(None, None, true, false, "custom-model");
    assert_eq!(r_levels, vec!["off", "low", "medium", "high"]);
    assert_eq!(r_def, "medium");

    let (r1_levels, r1_def) =
        resolve_thinking_levels(None, None, false, false, "deepseek-reasoner");
    assert_eq!(r1_levels, vec!["off", "low", "medium", "high"]);
    assert_eq!(r1_def, "high");

    let (o3_levels, o3_def) = resolve_thinking_levels(None, None, false, false, "o3-mini");
    assert_eq!(o3_levels, vec!["off", "low", "medium", "high"]);
    assert_eq!(o3_def, "medium");

    // 3. Normal non-reasoning model defaults to ["off"] and "off"
    let (norm_levels, norm_def) = resolve_thinking_levels(None, None, false, false, "gpt-4o-mini");
    assert_eq!(norm_levels, vec!["off"]);
    assert_eq!(norm_def, "off");

    // 4. Test parsing from JSON configuration
    let json = r#"{
        "providers": {
            "test-prov": {
                "baseUrl": "https://api.test.com/v1",
                "apiKey": "sk-123",
                "thinkingLevels": ["low", "medium"],
                "defaultThinkingLevel": "low",
                "models": [
                    { "id": "model-inherited" },
                    {
                        "id": "model-override",
                        "thinkingLevels": ["off", "high"],
                        "defaultThinkingLevel": "high"
                    }
                ]
            }
        }
    }"#;

    let models_file = thunder_agent_providers::config::ModelsFile::parse_json(json).unwrap();
    let auth = thunder_agent_providers::auth::AuthFile::default();
    let registry =
        thunder_agent_providers::catalog::ProviderRegistry::from_parts(models_file, &auth).unwrap();

    let m1 = registry.resolve("test-prov/model-inherited").unwrap();
    assert_eq!(m1.thinking_levels, vec!["low", "medium"]);
    assert_eq!(m1.default_thinking_level, "low");

    let m2 = registry.resolve("test-prov/model-override").unwrap();
    assert_eq!(m2.thinking_levels, vec!["off", "high"]);
    assert_eq!(m2.default_thinking_level, "high");
}

#[test]
fn test_models_file_config_extensions() {
    let json = r#"{
        "utilityModel": "command/xiaomi/mimo-v2.6-flash",
        "providers": {
            "cmd": {
                "baseUrl": "https://api.test.com/v1",
                "apiKey": "sk-123",
                "models": [
                    {
                        "id": "mimo",
                        "thinkingLevelMap": {
                            "off": "none",
                            "low": "low",
                            "medium": "medium",
                            "high": "high",
                            "xhigh": null,
                            "max": null
                        },
                        "thinkingLevelsProbed": true
                    }
                ]
            }
        }
    }"#;

    let models_file = thunder_agent_providers::config::ModelsFile::parse_json(json).unwrap();
    assert_eq!(
        models_file.utility_model.as_deref(),
        Some("command/xiaomi/mimo-v2.6-flash")
    );

    let model_cfg = &models_file.providers.get("cmd").unwrap().models[0];
    assert_eq!(model_cfg.thinking_levels_probed, Some(true));
    let effective = model_cfg.effective_thinking_levels().unwrap();
    assert_eq!(effective, vec!["off", "low", "medium", "high"]);
}

#[tokio::test]
async fn test_resolve_utility_model_and_probed_flag() {
    let json = r#"{
        "utilityModel": "cmd/flash-model",
        "providers": {
            "cmd": {
                "baseUrl": "https://api.test.com/v1",
                "apiKey": "sk-123",
                "models": [
                    {
                        "id": "heavy-reasoner",
                        "thinkingLevels": ["low", "high"],
                        "defaultThinkingLevel": "high",
                        "thinkingLevelsProbed": true
                    },
                    {
                        "id": "flash-model",
                        "thinkingLevels": ["off"]
                    }
                ]
            }
        }
    }"#;

    let models_file = thunder_agent_providers::config::ModelsFile::parse_json(json).unwrap();
    let auth = thunder_agent_providers::auth::AuthFile::default();
    let mut registry =
        thunder_agent_providers::catalog::ProviderRegistry::from_parts(models_file, &auth).unwrap();

    let utility = registry.resolve_utility_model().unwrap();
    assert_eq!(utility.id, "flash-model");

    let reasoner = registry.resolve("cmd/heavy-reasoner").unwrap();
    assert!(reasoner.thinking_levels_probed);
    assert_eq!(reasoner.thinking_levels, vec!["low", "high"]);

    // Calling probe_unprobed_models should skip already probed models
    registry.probe_unprobed_models().await;
    let reasoner_after = registry.resolve("cmd/heavy-reasoner").unwrap();
    assert_eq!(reasoner_after.thinking_levels, vec!["low", "high"]);
}
