use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;
use thunder_agent_loop::prelude::*;
use thunder_agent_loop::types::tool::{AgentTool, ToolExecutionContext};
use thunder_agent_plugin::{run_registry, SidecarConfig, SidecarManager, TsToolBridge};
use tokio_util::sync::CancellationToken;

/// Build a `SidecarConfig` for a test: one registered run, plus the sidecar-level
/// defaults the `call_rpc` seam uses.
///
/// Registering the run is the point: since a plugin's reverse RPC is authorised
/// against the run named in its `route`, a test that forgets to register one
/// would otherwise see every call refused for the wrong reason.
async fn config_for_test(
    ws_dir: &std::path::Path,
    plugins_dir: &std::path::Path,
    permission: Permission,
    host_ui: Option<Arc<dyn HostUi>>,
    tools: Option<Arc<dyn ToolInvoker>>,
) -> SidecarConfig {
    let runs = run_registry();
    let policy = Arc::new(SessionPolicy::new(permission, ApprovalMode::Never));
    runs.write()
        .await
        .begin_run(
            route(),
            ws_dir.to_path_buf(),
            Arc::clone(&policy),
            host_ui.clone(),
        )
        .await;
    // A working pipeline by default: the RPC layer delegates execution to it,
    // so a test that calls `ctx.exec` needs one.
    let tools = match tools {
        Some(tools) => Some(tools),
        None => Some(run_invoker_with(ws_dir, Arc::clone(&policy)).await),
    };
    if let Some(tools) = &tools {
        runs.write().await.set_tools(route(), tools.clone()).await;
    }
    SidecarConfig {
        runner_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runner")
            .join("host.mjs"),
        plugin_dirs: vec![plugins_dir.to_path_buf()],
        workspace_dir: ws_dir.to_path_buf(),
        runs,
        policy: Arc::new(tokio::sync::RwLock::new(Some(Arc::clone(&policy)))),
        host_ui: Arc::new(tokio::sync::RwLock::new(host_ui)),
        tool_invoker: Arc::new(tokio::sync::RwLock::new(tools)),
    }
}

/// A real tool pipeline for a run: the same one the host would build.
///
/// Plugin RPCs are now delegated to it rather than re-implemented, so a test
/// that exercises `ctx.exec` / `ctx.fs.*` has to supply one — which is also the
/// point. A sidecar with no invoker is a sidecar that cannot execute anything,
/// and saying so is the correct answer rather than a silent fallback.
async fn run_invoker_with(
    ws: &std::path::Path,
    policy: Arc<SessionPolicy>,
) -> Arc<dyn ToolInvoker> {
    use thunder_agent_loop::prelude::*;
    use thunder_agent_loop::tools::executor::ToolExecutor;
    use thunder_agent_loop::tools::middleware::ToolPipeline;
    use thunder_agent_loop::tools::registry::ToolRegistry;

    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(
        thunder_agent_loop::tools::builtin::WriteFileTool::default(),
    ));
    registry.register(Arc::new(
        thunder_agent_loop::tools::builtin::ReadFileTool::default(),
    ));
    registry.register(Arc::new(
        thunder_agent_loop::tools::builtin::BashTool::default(),
    ));
    let tier = policy.tier().await;
    let pipeline = ToolPipeline::configured(
        ws.to_path_buf(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        tier,
        Some(policy),
        Some(Arc::new(NullHostUi)),
    );
    let executor = ToolExecutor::with_pipeline(ToolRegistry::default(), pipeline);
    Arc::new(PipelineToolInvoker::new(executor).with_turn(1))
}

/// One policy, shared by the run registry and the pipeline.
///
/// This is what the host does — `ThunderRoot::execute` hands the same
/// `Arc<SessionPolicy>` to both — and it matters: if they were separate objects,
/// narrowing the run's policy would leave the pipeline judging on the old one.
async fn register_run(
    runs: &thunder_agent_plugin::RunRegistry,
    route: &str,
    ws: &std::path::Path,
    tier: Permission,
) {
    let policy = SessionPolicy::new(tier, ApprovalMode::Never);
    let invoker = run_invoker_with(ws, Arc::clone(&policy)).await;
    runs.write()
        .await
        .begin_run(route, ws.to_path_buf(), policy, Some(Arc::new(NullHostUi)))
        .await;
    runs.write().await.set_tools(route, invoker).await;
}

/// The route the tests' plugins are attributed to.
fn route() -> &'static str {
    "test-run"
}

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
    assert!(
        runner_path.exists(),
        "Runner script must exist at {:?}",
        runner_path
    );

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

    let config = config_for_test(&ws_dir, &plugins_dir, Permission::Bash, None, None).await;

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
        // Without a route the plugin's RPCs are refused — attribution is not
        // optional, it is the authorisation key.
        route: Some(route().to_string()),
        ..Default::default()
    };

    let result = bridge
        .execute(serde_json::json!({ "a": 6, "b": 7 }), &ctx)
        .await
        .expect("Execution should succeed");

    assert_eq!(result, "Product: 42");

    // Verify ctx.fs.writeFile created file atomically in workspace
    let written_file = ws_dir.join("result.txt");
    assert!(
        written_file.exists(),
        "result.txt should be written via atomic delegation"
    );
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

    assert_eq!(
        result_v2, "Sum: 30",
        "Should reflect updated hot-reloaded code"
    );

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

    assert_eq!(
        result_v3, "Sum: 10",
        "Should remain on working version despite syntax error"
    );
}

/// Security: a read-only role must not be bypassed through the plugin RPC
/// channel. This is the side channel that the host's tool gate cannot see.
#[tokio::test]
async fn read_only_permission_blocks_plugin_write_and_exec_rpc() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();

    let config = config_for_test(&ws_dir, &plugins_dir, Permission::Read, None, None).await;

    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");

    // Both RPCs must be refused before touching the filesystem or the shell.
    let write_err = sidecar
        .call_rpc(
            "fs_write_file",
            serde_json::json!({ "path": "should_not_exist.txt", "content": "nope" }),
        )
        .await
        .expect_err("fs_write_file must be denied under Read");
    assert!(
        write_err.contains("was not approved"),
        "the judge speaks now, not the old hand-rolled gate: {write_err}"
    );
    assert!(
        write_err.contains("read-only"),
        "and it should say why: {write_err}"
    );
    assert!(
        !ws_dir.join("should_not_exist.txt").exists(),
        "denied write must not create the file"
    );

    let exec_err = sidecar
        .call_rpc("exec_bash", serde_json::json!({ "command": "echo pwned" }))
        .await
        .expect_err("exec_bash must be denied under Read");
    assert!(
        exec_err.contains("was not approved") && exec_err.contains("read-only"),
        "unexpected error: {exec_err}"
    );
}

/// The regression the shared permission slot exists for: a sidecar booted
/// permissive must *narrow* when a later run arrives read-only.
///
/// Before the tier became a shared slot the sidecar captured `Permission::Bash`
/// once at boot, so every subsequent read-only role still got a full shell
/// through `ctx.exec()` — the host's tool gate never saw those calls.
#[tokio::test]
async fn permission_slot_narrows_a_running_sidecar() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();

    // Boot wide open, exactly as the old fixed-tier sidecar did.
    let config = config_for_test(&ws_dir, &plugins_dir, Permission::Bash, None, None).await;
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");

    let probe = ws_dir.join("narrow_probe.txt");
    sidecar
        .call_rpc(
            "fs_write_file",
            serde_json::json!({ "path": "narrow_probe.txt", "content": "before" }),
        )
        .await
        .expect("write allowed while the run is unrestricted");
    assert!(probe.exists());

    // Narrow the *run's* policy, which is the one the judge consults. Before
    // execution was delegated, the sidecar held a single tier and this was the
    // only lever; now there is a policy per run and this is it.
    let run = sidecar
        .runs()
        .read()
        .await
        .get("test-run")
        .await
        .expect("the run is registered");
    run.policy
        .as_ref()
        .expect("a run carries a policy")
        .set_tier(Permission::Read)
        .await;

    let err = sidecar
        .call_rpc(
            "fs_write_file",
            serde_json::json!({ "path": "narrow_probe.txt", "content": "after" }),
        )
        .await
        .expect_err("write must be denied once the run is read-only");
    assert!(
        err.contains("was not approved") && err.contains("read-only"),
        "got: {err}"
    );
    assert_eq!(
        tokio::fs::read_to_string(&probe).await.unwrap(),
        "before",
        "the denied write must not have landed"
    );
}

/// A TypeScript plugin must be able to raise a dialog, and the request must be
/// labelled as coming from a plugin — never as a host-initiated approval.
#[tokio::test]
async fn plugin_ui_reaches_the_host_labelled_as_plugin() {
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use thunder_agent_loop::types::config::Permission;
    use thunder_agent_loop::types::ui::{HostUi, NotifyLevel, UiRequest, UiResponse, UiSource};

    struct Recorder {
        seen: Arc<StdMutex<Vec<(UiSource, String, Vec<String>)>>>,
    }

    #[async_trait]
    impl HostUi for Recorder {
        async fn request(&self, source: UiSource, request: UiRequest) -> UiResponse {
            match request {
                UiRequest::Select { title, options, .. } => {
                    self.seen.lock().unwrap().push((source, title, options));
                    UiResponse::value("beta")
                }
                _ => UiResponse::Cancelled,
            }
        }
        fn notify(&self, _source: UiSource, _message: &str, _level: NotifyLevel) {}
        fn set_status(&self, _key: &str, _text: Option<String>) {}
    }

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("asker.ts"),
        r#"
export default definePlugin({
  name: "asker",
  tools: [{
    name: "ts_ask",
    description: "ask the user a question",
    execute: async (args, ctx) => {
      const choice = await ctx.ui.select("Which one?", ["alpha", "beta"]);
      ctx.ui.notify("asked", "info");
      return "chose:" + choice;
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    let seen = Arc::new(StdMutex::new(Vec::new()));
    let config = config_for_test(
        &ws_dir,
        &plugins_dir,
        Permission::Bash,
        Some(Arc::new(Recorder {
            seen: Arc::clone(&seen),
        })),
        None,
    )
    .await;
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");

    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_ask")
        .expect("plugin tool registered");
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        // Without a route the plugin's RPCs are refused — attribution is not
        // optional, it is the authorisation key.
        route: Some(route().to_string()),
        ..Default::default()
    };

    let out = bridge
        .execute(serde_json::json!({}), &ctx)
        .await
        .expect("plugin tool ran");
    assert_eq!(
        out, "chose:beta",
        "the plugin must receive the host's answer"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one dialog");
    assert_eq!(seen[0].0, UiSource::Plugin, "must be labelled as a plugin");
    assert_eq!(seen[0].2, vec!["alpha", "beta"]);
}

/// With no panel attached, a plugin dialog must resolve as "no", not hang and
/// not succeed.
#[tokio::test]
async fn plugin_ui_without_a_panel_fails_closed() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("silent.ts"),
        r#"
export default definePlugin({
  name: "silent",
  tools: [{
    name: "ts_confirm",
    description: "confirm",
    execute: async (args, ctx) => "answer:" + (await ctx.ui.confirm("Sure?", "really"))
  }]
});
"#,
    )
    .await
    .unwrap();

    let config = config_for_test(&ws_dir, &plugins_dir, Permission::Bash, None, None).await;
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_confirm")
        .expect("registered");
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        // Without a route the plugin's RPCs are refused — attribution is not
        // optional, it is the authorisation key.
        route: Some(route().to_string()),
        ..Default::default()
    };

    if let Ok(out) = bridge.execute(serde_json::json!({}), &ctx).await {
        assert_ne!(out, "answer:true", "must never report consent");
    }
}

/// End-to-end: a TypeScript plugin calls a *host* tool through `ctx.callTool`,
/// and the call lands in the workspace through the host's pipeline.
#[tokio::test]
async fn plugin_call_tool_reaches_a_registered_tool() {
    use thunder_agent_loop::prelude::*;
    use thunder_agent_loop::tools::executor::ToolExecutor;
    use thunder_agent_loop::tools::middleware::ToolPipeline;
    use thunder_agent_loop::tools::registry::ToolRegistry;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("writer.ts"),
        r#"
export default definePlugin({
  name: "writer",
  tools: [{
    name: "ts_write_via_host",
    description: "delegate the write to the host's write_file",
    execute: async (args, ctx) => {
      return await ctx.callTool("write_file", { path: "delegated.txt", content: "from-plugin" });
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    // The host's real pipeline: write_file registered, approval gate installed
    // but in the default no-prompt mode.
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    registry.register(Arc::new(
        thunder_agent_loop::tools::builtin::WriteFileTool::default(),
    ));
    let pipeline = ToolPipeline::configured(
        ws_dir.clone(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(SessionPolicy::new(Permission::Bash, ApprovalMode::Never)),
        Some(Arc::new(NullHostUi)),
    );
    let executor = ToolExecutor::with_pipeline(ToolRegistry::default(), pipeline);
    let invoker: Arc<dyn ToolInvoker> = Arc::new(PipelineToolInvoker::new(executor).with_turn(1));

    let config =
        config_for_test(&ws_dir, &plugins_dir, Permission::Bash, None, Some(invoker)).await;

    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_write_via_host")
        .expect("registered");
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        // Without a route the plugin's RPCs are refused — attribution is not
        // optional, it is the authorisation key.
        route: Some(route().to_string()),
        ..Default::default()
    };

    bridge
        .execute(serde_json::json!({}), &ctx)
        .await
        .expect("the delegated call must succeed");

    let landed = ws_dir.join("delegated.txt");
    assert!(
        landed.exists(),
        "ctx.callTool must reach the host's write_file for real"
    );
    assert_eq!(
        tokio::fs::read_to_string(&landed).await.unwrap(),
        "from-plugin"
    );
}

/// Sibling TypeScript plugin tool calls (`ctx.callTool` to another TS plugin tool)
/// must route through Rust's ToolPipeline rather than bypassing via Node in-memory shortcuts.
#[tokio::test]
async fn ts_plugin_calling_sibling_ts_plugin_routes_through_pipeline() {
    use thunder_agent_loop::prelude::*;
    use thunder_agent_loop::tools::executor::ToolExecutor;
    use thunder_agent_loop::tools::middleware::ToolPipeline;
    use thunder_agent_loop::tools::registry::ToolRegistry;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();

    // Plugin A calls Plugin B's tool
    tokio::fs::write(
        plugins_dir.join("plugin_a.ts"),
        r#"
export default definePlugin({
  name: "plugin_a",
  tools: [{
    name: "tool_caller",
    description: "calls tool from sibling plugin b",
    execute: async (args, ctx) => {
      const res = await ctx.callTool("tool_calculator", { a: 10, b: 25 });
      return `result: ${res}`;
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    // Plugin B provides tool_calculator
    tokio::fs::write(
        plugins_dir.join("plugin_b.ts"),
        r#"
export default definePlugin({
  name: "plugin_b",
  tools: [{
    name: "tool_calculator",
    description: "adds two numbers",
    execute: async (args, ctx) => {
      return String(Number(args.a) + Number(args.b));
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_secs(5));
    let invoker_slot: Arc<tokio::sync::RwLock<Option<Arc<dyn ToolInvoker>>>> =
        Arc::new(tokio::sync::RwLock::new(None));

    struct DelegatingInvoker(Arc<tokio::sync::RwLock<Option<Arc<dyn ToolInvoker>>>>);
    #[async_trait::async_trait]
    impl ToolInvoker for DelegatingInvoker {
        async fn invoke(
            &self,
            tool: &str,
            args: serde_json::Value,
            ctx: &ToolInvocationContext,
        ) -> Result<String, String> {
            let inner = self.0.read().await.clone().expect("invoker ready");
            inner.invoke(tool, args, ctx).await
        }
    }

    let invoker: Arc<dyn ToolInvoker> = Arc::new(DelegatingInvoker(Arc::clone(&invoker_slot)));
    let config =
        config_for_test(&ws_dir, &plugins_dir, Permission::Bash, None, Some(invoker)).await;

    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if sidecar.list_tools().await.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let all_tools = sidecar.list_tools().await;
    assert_eq!(all_tools.len(), 2, "both plugins loaded");

    for t in all_tools {
        registry.register(Arc::new(TsToolBridge::new(t, Arc::clone(&sidecar))));
    }

    let pipeline = ToolPipeline::configured(
        ws_dir.clone(),
        &[],
        registry,
        None,
        &thunder_agent_loop::types::config::MiddlewareConfig::default(),
        Permission::Bash,
        Some(SessionPolicy::new(Permission::Bash, ApprovalMode::Never)),
        Some(Arc::new(NullHostUi)),
    );
    let executor = ToolExecutor::with_pipeline(ToolRegistry::default(), pipeline);
    *invoker_slot.write().await = Some(Arc::new(PipelineToolInvoker::new(executor).with_turn(1)));

    let caller_tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "tool_caller")
        .expect("registered");
    let bridge = TsToolBridge::new(caller_tool, Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "call_ab".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        route: Some(route().to_string()),
        ..Default::default()
    };

    let result = bridge
        .execute(serde_json::json!({}), &ctx)
        .await
        .expect("inter-plugin call must succeed via Rust pipeline");

    assert_eq!(result, "result: 35");
}

/// The regression this whole refactor exists for: two *concurrent* runs sharing
/// one sidecar must not see each other's authority.
///
/// Before per-run services, the sidecar held a single permission tier, so
/// whichever run started last decided what both could do. Read-only run A could
/// then find `ctx.exec()` working because bash run B had overwritten the slot.
#[tokio::test]
async fn concurrent_runs_do_not_share_authority() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_a = temp.path().join("run-a");
    let ws_b = temp.path().join("run-b");
    let plugins_dir = temp.path().join("plugins");
    tokio::fs::create_dir_all(&ws_a).await.unwrap();
    tokio::fs::create_dir_all(&ws_b).await.unwrap();
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("writer.ts"),
        r#"
export default definePlugin({
  name: "writer",
  tools: [{
    name: "ts_write",
    description: "write into the run's own workspace",
    execute: async (args, ctx) => {
      await ctx.fs.writeFile("out.txt", ctx.sessionId || "unknown");
      return "ok";
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    // One registry, two runs — exactly the sharing the daemon creates.
    let runs = run_registry();
    register_run(&runs, "run-a", &ws_a, Permission::Read).await;
    register_run(&runs, "run-b", &ws_b, Permission::Bash).await;

    let config = SidecarConfig {
        runner_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runner")
            .join("host.mjs"),
        plugin_dirs: vec![plugins_dir.clone()],
        workspace_dir: temp.path().to_path_buf(),
        runs,
        policy: Arc::new(tokio::sync::RwLock::new(None)),
        host_ui: Arc::new(tokio::sync::RwLock::new(None)),
        tool_invoker: Arc::new(tokio::sync::RwLock::new(None)),
    };
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_write")
        .expect("registered");
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));

    let run = |route: &'static str| ToolExecutionContext {
        tool_call_id: format!("c-{route}"),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        route: Some(route.to_string()),
        ..Default::default()
    };

    // The permissive run works...
    bridge
        .execute(serde_json::json!({}), &run("run-b"))
        .await
        .expect("run-b is a bash run and may write");
    assert!(
        ws_b.join("out.txt").exists(),
        "run-b wrote into its own workspace"
    );
    assert!(
        !ws_a.join("out.txt").exists(),
        "run-b must not write into run-a's workspace"
    );

    // ...and the read-only run is still refused, despite sharing the sidecar.
    let denied = bridge.execute(serde_json::json!({}), &run("run-a")).await;
    assert!(
        denied.is_err(),
        "run-a is read-only; sharing a sidecar must not grant it bash"
    );
    let err = denied.unwrap_err();
    assert!(
        err.contains("was not approved") && err.contains("read-only"),
        "got: {err}"
    );
    assert!(
        !ws_a.join("out.txt").exists(),
        "the refused write must not land anywhere"
    );
}

/// A call that names no run is refused, rather than falling back to whatever
/// authority happens to be registered.
#[tokio::test]
async fn an_unattributed_call_is_refused() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("writer.ts"),
        r#"
export default definePlugin({
  name: "writer",
  tools: [{
    name: "ts_write",
    description: "write",
    execute: async (args, ctx) => { await ctx.fs.writeFile("out.txt", "x"); return "ok"; }
  }]
});
"#,
    )
    .await
    .unwrap();

    let runs = run_registry();
    register_run(&runs, "run-a", &ws_dir, Permission::Bash).await;

    let config = SidecarConfig {
        runner_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runner")
            .join("host.mjs"),
        plugin_dirs: vec![plugins_dir.clone()],
        workspace_dir: ws_dir.clone(),
        runs,
        policy: Arc::new(tokio::sync::RwLock::new(None)),
        host_ui: Arc::new(tokio::sync::RwLock::new(None)),
        tool_invoker: Arc::new(tokio::sync::RwLock::new(None)),
    };
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_write")
        .unwrap();
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));

    // A tool call with no route: the RPC cannot be attributed, so it is refused
    // even though a permissive run is registered.
    let unrouted = ToolExecutionContext {
        tool_call_id: "c-unrouted".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        ..Default::default()
    };
    let err = bridge
        .execute(serde_json::json!({}), &unrouted)
        .await
        .expect_err("an unattributed call must not run");
    assert!(err.contains("run identifier"), "got: {err}");
    assert!(!ws_dir.join("out.txt").exists());
}

/// A plugin must not be able to run a command the model could not run.
///
/// `SecurityGuardMiddleware` refuses a fixed set of destructive commands for
/// `bash`. A TypeScript plugin's `ctx.exec()` does not go through the tool
/// pipeline at all — it hand-rolls `bash -c` — so the same command is accepted
/// there. That makes "the agent cannot `rm -rf /`" false, and a plugin is the
/// cheapest way to prove it.
///
/// This was red (`#[ignore]`d) while the plugin RPC path re-implemented `bash`
/// by hand. It is the acceptance criterion for folding that path into the
/// pipeline: the plugin now gets the same guard the model does.
#[tokio::test]
async fn plugin_cannot_run_a_command_the_guard_forbids() {
    use thunder_agent_loop::types::config::Permission;

    if !SidecarManager::is_node_available().await {
        eprintln!("Node.js not available, skipping test");
        return;
    }

    let temp = tempdir().unwrap();
    let ws_dir = temp.path().to_path_buf();
    let plugins_dir = ws_dir.join(".arp").join("plugins");
    tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
    tokio::fs::write(
        plugins_dir.join("execer.ts"),
        r#"
export default definePlugin({
  name: "execer",
  tools: [{
    name: "ts_exec",
    description: "run a shell command",
    execute: async (args, ctx) => {
      const r = await ctx.exec("echo definitely-not-pwned");
      return "exit=" + r.exitCode;
    }
  }]
});
"#,
    )
    .await
    .unwrap();

    let runs = run_registry();
    register_run(&runs, "run-x", &ws_dir, Permission::Bash).await;

    let config = SidecarConfig {
        runner_path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("runner")
            .join("host.mjs"),
        plugin_dirs: vec![plugins_dir.clone()],
        workspace_dir: ws_dir.clone(),
        runs,
        policy: Arc::new(tokio::sync::RwLock::new(None)),
        host_ui: Arc::new(tokio::sync::RwLock::new(None)),
        tool_invoker: Arc::new(tokio::sync::RwLock::new(None)),
    };
    let sidecar = SidecarManager::new(config);
    sidecar.start().await.expect("start sidecar");
    for _ in 0..100 {
        if !sidecar.list_tools().await.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // A safe command proves the plumbing works, so a failure below is about the
    // guard rather than about the RPC being unreachable.
    let tool = sidecar
        .list_tools()
        .await
        .into_iter()
        .find(|t| t.name == "ts_exec")
        .expect("registered");
    let bridge = TsToolBridge::new(tool, Arc::clone(&sidecar));
    let ctx = ToolExecutionContext {
        tool_call_id: "c1".into(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
        route: Some("run-x".to_string()),
        ..Default::default()
    };
    bridge
        .execute(serde_json::json!({}), &ctx)
        .await
        .expect("a harmless command still runs");

    // Now one the model cannot run. `dd if=` is on SecurityGuard's forbidden list
    // for every tier.
    //
    // Deliberately not `rm -rf /`: the OS refuses that one ("/ may not be
    // removed"), so it would pass for the wrong reason and hide the gap. `dd`
    // into the workspace is survivable, unguarded by the OS, and destructive.
    let forbidden = sidecar
        .call_rpc(
            "exec_bash",
            serde_json::json!({
                "command": "dd if=/dev/zero of=dd_probe bs=1m count=8"
            }),
        )
        .await;
    assert!(
        forbidden.is_err(),
        "a forbidden command must be refused on the plugin path too, got: {forbidden:?}"
    );
    assert!(
        !ws_dir.join("dd_probe").exists(),
        "the refused command must not have created its target"
    );
}
