use thunder_agent_root::prelude::*;

#[tokio::test]
async fn test_plugin_registry_registration_and_retrieval() {
    let mut registry = PluginRegistry::new();

    let conv_plugin = ConversationPlugin::with_memory_store();
    let skills_plugin = SkillsPlugin::default();
    let mcp_plugin = McpPlugin::default();

    registry.register(conv_plugin);
    registry.register(skills_plugin);
    registry.register(mcp_plugin);

    assert_eq!(registry.list().len(), 3);
    assert!(registry.get("conversation").is_some());
    assert!(registry.get("skills").is_some());
    assert!(registry.get("mcp").is_some());
    assert!(registry.get("unknown").is_none());

    let manifests = registry.list_manifests();
    assert_eq!(manifests.len(), 3);

    let mem_plugins = registry.find_by_capability(&PluginCapability::MemoryPersistence);
    assert_eq!(mem_plugins.len(), 1);
    assert_eq!(mem_plugins[0].manifest().id, "conversation");

    let tool_plugins = registry.find_by_capability(&PluginCapability::ToolProvider);
    assert_eq!(tool_plugins.len(), 2); // skills, mcp
}

#[tokio::test]
async fn test_active_plugin_set_tool_collection_and_prompt_assembly() {
    let mut registry = PluginRegistry::new();
    registry.register(ConversationPlugin::with_memory_store());
    registry.register(SkillsPlugin::default());

    let active_set = registry.create_active_set(&["skills".to_string(), "conversation".to_string()]);
    assert_eq!(active_set.plugins().len(), 2);

    let prompt = active_set.build_combined_system_prompt(Some("Base Assistant"));
    assert!(prompt.contains("Base Assistant"));
    assert!(prompt.contains("Session history and multi-turn state"));
}
