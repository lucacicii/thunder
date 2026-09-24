use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_plugin::{SidecarConfig, SidecarManager, TsToolBridge};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_ts_plugin_lifecycle_and_hot_reload() {
    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();

    let runner_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("runner")
        .join("host.mjs");
    assert!(runner_path.exists(), "Runner script must exist at {:?}", runner_path);

    // 1. Create a sample TypeScript plugin
    let plugin_file = plugins_dir.join("calc_plugin.ts");
    let initial_ts = r#"
export default definePlugin({
  name: "calc_plugin",
  version: "1.0.0",
  description: "A calculator and note writer in TS",
  systemPrompt: async (ctx) => {
    return "TS Calculator is active.";
  },
  tools: [
    {
      name: "ts_multiply",
      description: "Multiply two numbers",
      parameters: {
        type: "object",
        properties: {
          a: { type: "number" },
          b: { type: "number" }
        }
      },
      execute: async (args, ctx) => {
        const res = args.a * args.b;
        // Test ctx.fs.writeFile via Rust atomic delegation
        await ctx.fs.writeFile("result.txt", `Result: ${res}`);
        return `Product: ${res}`;
      }
    }
  ]
});
"#;
    tokio::fs::write(&plugin_file, initial_ts).await.unwrap();

    let config = SidecarConfig {
        runner_path,
        plugin_dirs: vec![plugins_dir.clone()],
        workspace_dir: ws_dir.clone(),
    };

    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("Failed to start sidecar");

    // Wait up to 2 seconds for manifest synchronization
    let mut tools = Vec::new();
    for _ in 0..20 {
        tools = sidecar.list_tools().await;
        if !tools.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert_eq!(tools.len(), 1, "Should have discovered 1 tool");
    let tool_meta = tools[0].clone();
    assert_eq!(tool_meta.name, "ts_multiply");

    // 2. Test Capability B: System Prompt
    let prompts = sidecar.get_system_prompts(serde_json::json!({})).await;
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].prompt, "TS Calculator is active.");

    // 3. Test Capability C: Execute Tool via TsToolBridge
    let bridge = TsToolBridge::new(tool_meta.clone(), Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "test_call_1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    };

    let result = bridge
        .execute(serde_json::json!({ "a": 6, "b": 7 }), &ctx)
        .await
        .expect("Execution should succeed");

    assert_eq!(result, "Product: 42");

    // Verify ctx.fs.writeFile created file atomically in workspace
    let written_file = ws_dir.join("result.txt");
    assert!(written_file.exists(), "result.txt should be written via atomic delegation");
    let content = tokio::fs::read_to_string(&written_file).await.unwrap();
    assert_eq!(content, "Result: 42");

    // 4. Test Blue-Green Hot-Reload: Update plugin logic to add addition
    let updated_ts = r#"
export default definePlugin({
  name: "calc_plugin",
  version: "1.1.0",
  tools: [
    {
      name: "ts_multiply",
      description: "Updated to add instead",
      execute: async (args, ctx) => {
        return `Sum: ${args.a + args.b}`;
      }
    }
  ]
});
"#;
    tokio::fs::write(&plugin_file, updated_ts).await.unwrap();

    // Trigger reload
    sidecar.reload(Some(plugin_file.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Execute again to confirm new blue-green version is active
    let result_v2 = bridge
        .execute(serde_json::json!({ "a": 10, "b": 20 }), &ctx)
        .await
        .expect("Execution should succeed");

    assert_eq!(result_v2, "Sum: 30", "Should reflect updated hot-reloaded code");

    // 5. Test Syntax Error Immunity: Saving invalid code does NOT crash or unload
    let invalid_ts = r#"
export default definePlugin({
  name: "calc_plugin",
  tools: [ { name: "ts_multiply", execute: (args => { // SYNTAX ERROR: unclosed
"#;
    tokio::fs::write(&plugin_file, invalid_ts).await.unwrap();
    sidecar.reload(Some(plugin_file.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Previous version should still be operational (error immunity)
    let result_v3 = bridge
        .execute(serde_json::json!({ "a": 5, "b": 5 }), &ctx)
        .await
        .expect("Execution should continue using prior active version");

    assert_eq!(result_v3, "Sum: 10", "Should remain on working version despite syntax error");
}
