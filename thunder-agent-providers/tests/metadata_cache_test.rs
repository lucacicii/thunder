use tempfile::tempdir;
use thunder_agent_providers::prelude::*;

#[test]
fn test_metadata_cache_resolution_hierarchy() {
    let dir = tempdir().expect("tempdir");
    let cache_file = dir.path().join("models_metadata.json");

    let mut cache = ModelMetadataCache::default();
    cache
        .context_windows
        .insert("custom-provider/my-model".to_string(), 65536);
    cache.save_to_file(&cache_file).expect("save cache");

    let loaded_cache = ModelMetadataCache::load_from_file(&cache_file);

    // 1. Explicit configuration takes highest priority
    let explicit = loaded_cache.resolve_context_window(
        "custom-provider/my-model",
        "my-model",
        Some(1_000_000),
    );
    assert_eq!(explicit, 1_000_000);

    // 2. Cached learned limit is used when no explicit config is present
    let from_cache =
        loaded_cache.resolve_context_window("custom-provider/my-model", "my-model", None);
    assert_eq!(from_cache, 65536);

    // 3. Fallback to safe default when completely unknown
    let unknown = loaded_cache.resolve_context_window("other/unknown-model", "unknown-model", None);
    assert_eq!(unknown, DEFAULT_SAFE_CONTEXT_WINDOW);
}

#[tokio::test]
async fn test_provider_registry_update_model_context_window() {
    let mut registry = ProviderRegistry::default();
    registry.models.push(ModelSpec {
        provider: "mock".to_string(),
        id: "model-x".to_string(),
        name: "Model X".to_string(),
        api: ProviderApi::OpenAiCompletions,
        base_url: "http://localhost".to_string(),
        api_key: None,
        headers: std::collections::HashMap::new(),
        reasoning: false,
        context_window: 128_000,
        max_tokens: 4096,
        available: true,
        supports_developer_role: false,
        supports_reasoning_effort: false,
        max_tokens_field: "max_tokens".to_string(),
        thinking_levels: vec!["off".to_string()],
        default_thinking_level: "off".to_string(),
        thinking_levels_probed: false,
        thinking_level_map: None,
        compat: None,
    });

    assert_eq!(registry.models[0].context_window, 128_000);

    // Dynamically learn new context limit from 400 detection
    let updated = registry.update_model_context_window("mock/model-x", 32_768);
    assert!(updated);
    assert_eq!(registry.models[0].context_window, 32_768);
}
