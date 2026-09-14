use thunder_agent_root::prelude::*;

#[tokio::test]
async fn test_heuristic_plugin_selection_triggers() {
    let mut registry = PluginRegistry::new();
    registry.register(ConversationPlugin::with_memory_store());
    registry.register(SkillsPlugin::default());
    registry.register(McpPlugin::default());

    let selector = PluginSelector::new(None);
    let manifests = registry.list_manifests();

    // 1. Trigger skills
    let res1 = selector.heuristic_select("Can you use a custom skill or playbook for this task?", &manifests);
    assert!(res1.active_plugin_ids.contains(&"skills".to_string()));

    // 2. Trigger mcp
    let res2 = selector.heuristic_select("Connect to external MCP server to list tools", &manifests);
    assert!(res2.active_plugin_ids.contains(&"mcp".to_string()));

    // 3. Trigger conversation history
    let res3 = selector.heuristic_select("Show previous conversation history from this session", &manifests);
    assert!(res3.active_plugin_ids.contains(&"conversation".to_string()));

    // 4. Default fallback when no keyword matches
    let res4 = selector.heuristic_select("What is 1 + 1?", &manifests);
    assert!(res4.active_plugin_ids.contains(&"conversation".to_string()));
}
