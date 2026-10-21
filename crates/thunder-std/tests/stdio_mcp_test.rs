use serde_json::json;
use std::time::Duration;
use tempfile::tempdir;
use thunder_executor::{McpServerConfig, McpToolProvider};
use thunder_protocol::{ExecutionContext, ToolProvider};

#[tokio::test]
async fn thunder_std_mcp_process_executes_all_core_tools_with_context_envelope() {
    let bin_path = env!("CARGO_BIN_EXE_thunder-std");
    let cfg = McpServerConfig::new(bin_path).as_core();
    let provider = McpToolProvider::connect("thunder-std", &cfg)
        .await
        .expect("connect to thunder-std");

    let tools = provider.list_tools().await.expect("list_tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"core__read_file"));
    assert!(names.contains(&"core__write_file"));
    assert!(names.contains(&"core__edit_block"));
    assert!(names.contains(&"core__bash"));
    assert!(names.contains(&"core__grep"));
    assert!(names.contains(&"core__find"));
    assert!(names.contains(&"core__ls"));

    // Verify Context Envelope (`_meta.workspace_cwd`) isolates two sessions on the same long-lived process.
    let ws_a = tempdir().unwrap();
    let ws_b = tempdir().unwrap();
    let ctx_a = ExecutionContext::new("sess-a", ws_a.path(), Duration::from_secs(10));
    let ctx_b = ExecutionContext::new("sess-b", ws_b.path(), Duration::from_secs(10));

    provider
        .call_tool(
            "core__write_file",
            json!({
                "path": "src/hello.txt",
                "content": "alpha\nbeta\ngamma\n"
            }),
            &ctx_a,
        )
        .await
        .expect("write_file in ws_a");

    provider
        .call_tool(
            "core__write_file",
            json!({
                "path": "src/hello.txt",
                "content": "isolated-b\n"
            }),
            &ctx_b,
        )
        .await
        .expect("write_file in ws_b");

    // Edit ws_a file using edit_block
    provider
        .call_tool(
            "core__edit_block",
            json!({
                "path": "src/hello.txt",
                "old_text": "beta",
                "new_text": "BETA_EDITED"
            }),
            &ctx_a,
        )
        .await
        .expect("edit_block in ws_a");

    let read_a = provider
        .call_tool(
            "core__read_file",
            json!({ "path": "src/hello.txt" }),
            &ctx_a,
        )
        .await
        .expect("read_file in ws_a");
    assert_eq!(read_a, "alpha\nBETA_EDITED\ngamma\n");

    let read_b = provider
        .call_tool(
            "core__read_file",
            json!({ "path": "src/hello.txt" }),
            &ctx_b,
        )
        .await
        .expect("read_file in ws_b");
    assert_eq!(read_b, "isolated-b\n");

    // Execute bash inside ws_a via _meta.workspace_cwd
    let bash_out = provider
        .call_tool(
            "core__bash",
            json!({ "command": "cat src/hello.txt | grep BETA_EDITED" }),
            &ctx_a,
        )
        .await
        .expect("bash in ws_a");
    assert_eq!(bash_out.trim(), "BETA_EDITED");

    // Verify native `grep`, `find`, and `ls` in ws_a
    let grep_out = provider
        .call_tool("core__grep", json!({ "pattern": "BETA_EDITED" }), &ctx_a)
        .await
        .expect("grep in ws_a");
    assert!(grep_out.contains("src/hello.txt:2:BETA_EDITED"));

    let find_out = provider
        .call_tool("core__find", json!({ "pattern": "*.txt" }), &ctx_a)
        .await
        .expect("find in ws_a");
    assert_eq!(find_out.trim(), "src/hello.txt");

    let ls_out = provider
        .call_tool("core__ls", json!({}), &ctx_a)
        .await
        .expect("ls in ws_a");
    assert!(ls_out.contains("src/"));

    // Verify pagination with offset & limit
    let paged_out = provider
        .call_tool(
            "core__read_file",
            json!({ "path": "src/hello.txt", "offset": 2, "limit": 1 }),
            &ctx_a,
        )
        .await
        .expect("read_file paged");
    assert_eq!(paged_out.trim(), "BETA_EDITED");
}
