use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::ToolExecutionContext;
use thunder_agent_loop::AgentConfig;
use thunder_agent_pack_code::builtin::{
    BashPlugin, FindPlugin, GrepPlugin, ListDirPlugin, ReadFilePlugin, WriteFilePlugin,
};

#[tokio::test]
async fn test_mcp_plugins_direct_registration_and_execution() {
    let temp = tempdir().unwrap();
    let temp_path = temp.path().to_path_buf();

    let mut agent = AgentLoop::new(AgentConfig::default());

    // Register all pack-code plugins via register_plugin (as ToolPlugin)
    agent.register_plugin(Arc::new(
        WriteFilePlugin::default().with_default_cwd(temp_path.clone()),
    ));
    agent.register_plugin(Arc::new(
        ReadFilePlugin::default().with_default_cwd(temp_path.clone()),
    ));
    agent.register_plugin(Arc::new(
        GrepPlugin::default().with_default_cwd(temp_path.clone()),
    ));
    agent.register_plugin(Arc::new(
        FindPlugin::default().with_default_cwd(temp_path.clone()),
    ));
    agent.register_plugin(Arc::new(
        ListDirPlugin::default().with_default_cwd(temp_path.clone()),
    ));
    agent.register_plugin(Arc::new(
        BashPlugin::default().with_default_cwd(temp_path.clone()),
    ));

    let executor = agent.tool_executor().clone();

    // 1. write_file
    let write_call = ToolCall::new_function(
        "call_w",
        "write_file",
        json!({ "path": "hello.txt", "content": "Hello MCP World!" }).to_string(),
    );
    let res = executor
        .run_tool(&write_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "write failed: {}", res.output);

    // 2. read_file
    let read_call = ToolCall::new_function(
        "call_r",
        "read_file",
        json!({ "path": "hello.txt" }).to_string(),
    );
    let res = executor
        .run_tool(&read_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "read failed: {}", res.output);
    assert_eq!(res.output, "Hello MCP World!");

    // 3. grep
    let grep_call =
        ToolCall::new_function("call_g", "grep", json!({ "pattern": "MCP" }).to_string());
    let res = executor
        .run_tool(&grep_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "grep failed: {}", res.output);
    assert!(res.output.contains("Hello MCP World!"));

    // 4. find
    let find_call =
        ToolCall::new_function("call_f", "find", json!({ "pattern": "*.txt" }).to_string());
    let res = executor
        .run_tool(&find_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "find failed: {}", res.output);
    assert!(res.output.contains("hello.txt"));

    // 5. ls
    let ls_call = ToolCall::new_function("call_ls", "ls", json!({}).to_string());
    let res = executor
        .run_tool(&ls_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "ls failed: {}", res.output);
    assert!(res.output.contains("hello.txt"));

    // 6. bash
    let bash_call = ToolCall::new_function(
        "call_b",
        "bash",
        json!({ "command": "echo 'running bash'" }).to_string(),
    );
    let res = executor
        .run_tool(&bash_call, ToolExecutionContext::default(), None)
        .await;
    assert!(!res.is_error, "bash failed: {}", res.output);
    assert!(res.output.contains("running bash"));
}
