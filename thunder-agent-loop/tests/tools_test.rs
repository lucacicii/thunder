use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::tools::builtin::bash::BashTool;
use thunder_agent_loop::tools::executor::ToolExecutor;
use thunder_agent_loop::tools::registry::ToolRegistry;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio_util::sync::CancellationToken;

struct MultiplierTool;

#[async_trait]
impl AgentTool for MultiplierTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "multiply",
            "Multiply two numbers",
            json!({
                "type": "object",
                "properties": {
                    "a": { "type": "number" },
                    "b": { "type": "number" }
                },
                "required": ["a", "b"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let a = args
            .get("a")
            .and_then(|v| v.as_f64())
            .ok_or("Missing 'a'")?;
        let b = args
            .get("b")
            .and_then(|v| v.as_f64())
            .ok_or("Missing 'b'")?;
        Ok(json!({ "result": a * b }).to_string())
    }
}

#[tokio::test]
async fn test_tool_registry_and_executor() {
    let mut registry = ToolRegistry::new(1024, Duration::from_secs(5));
    registry.register(Arc::new(MultiplierTool));

    let executor = ToolExecutor::new(registry);
    let calls = vec![
        ToolCall::new_function("call_1", "multiply", "{\"a\": 6, \"b\": 7}"),
        ToolCall::new_function("call_2", "multiply", "{\"a\": 10, \"b\": 20}"),
    ];

    let results = executor
        .execute_all(&calls, 1, CancellationToken::new(), None)
        .await;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].result.output, "{\"result\":42.0}");
    assert_eq!(results[1].result.output, "{\"result\":200.0}");
    assert!(!results[0].result.is_error);
    assert!(!results[1].result.is_error);
}

#[tokio::test]
async fn test_bash_tool_execution() {
    let bash = BashTool::default();
    let ctx = ToolExecutionContext {
        tool_call_id: "test_1".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    };

    let res = bash
        .execute(json!({ "command": "echo 'thunder-agent-loop-test'" }), &ctx)
        .await;

    assert!(res.is_ok());
    assert_eq!(res.unwrap().trim(), "thunder-agent-loop-test");

    // Test with cwd
    let res_cwd = bash
        .execute(json!({ "command": "pwd", "cwd": "/" }), &ctx)
        .await;
    assert!(res_cwd.is_ok());
    assert_eq!(res_cwd.unwrap().trim(), "/");

    // Test with timeout_ms
    let res_timeout = bash
        .execute(json!({ "command": "sleep 5", "timeout_ms": 100 }), &ctx)
        .await;
    assert!(res_timeout.is_err());
    assert!(res_timeout.unwrap_err().contains("timeout"));
}

#[tokio::test]
async fn test_agent_loop_with_builtins_builder() {
    let cfg = thunder_agent_loop::types::config::AgentConfig::new("mock-model");
    let agent = thunder_agent_loop::AgentLoop::new(cfg).with_builtins();
    // AgentLoop successfully built with bash, read_file, and write_file
    assert_eq!(
        agent.status(),
        thunder_agent_loop::core::state::LoopStatus::Idle
    );
}

#[tokio::test]
async fn test_turn_off_transactions_without_modifying_code() {
    // Option 1: Using AgentConfig builder
    let cfg1 =
        thunder_agent_loop::types::config::AgentConfig::new("mock-model").without_transactions();
    assert!(!cfg1.middleware.enable_transaction);

    // Option 2: Using direct boolean toggle in config
    let mut cfg2 = thunder_agent_loop::types::config::AgentConfig::new("mock-model");
    cfg2.middleware.enable_transaction = false;
    assert!(!cfg2.middleware.enable_transaction);

    // Option 3: Using AgentLoop fluent method
    let agent = thunder_agent_loop::AgentLoop::new(cfg2).without_transactions();
    assert!(!agent.config().middleware.enable_transaction);

    // Option 4: Disabling all middlewares for bare-metal execution
    let cfg_bare =
        thunder_agent_loop::types::config::AgentConfig::new("mock-model").without_middlewares();
    assert!(!cfg_bare.middleware.enable_transaction);
    assert!(!cfg_bare.middleware.enable_security_guard);
    assert!(!cfg_bare.middleware.enable_resource_guard);
    assert!(!cfg_bare.middleware.enable_output_post_processor);
}

// ════════════════════════════════════════════════════════════
// grep / find / ls — embedded ripgrep-core tools
// ════════════════════════════════════════════════════════════

use thunder_agent_loop::tools::builtin::fs::ReadFileTool;
use thunder_agent_loop::tools::builtin::fs_ext::{FindTool, ListDirTool};
use thunder_agent_loop::tools::builtin::search::GrepTool;

fn test_ctx() -> ToolExecutionContext {
    ToolExecutionContext {
        tool_call_id: "grep_test".to_string(),
        turn: 1,
        cancellation_token: CancellationToken::new(),
    }
}

/// Creates:
/// ```text
/// root/
///   .gitignore        → "vendor/\n"
///   src/a.ts          → "hello alpha\nsecond line\nhello again\n"
///   src/b.ts          → "hello beta\n"
///   docs/note.md      → "hello gamma\n"
///   src/c.ts          → "hello one\nmid\nmid2\nmid3\nhello five\n"
///   vendor/ignored.ts → "hello hidden\n"   (gitignored)
///   blob.bin          → "hello \0 binary \n" (binary)
/// ```
fn grep_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    std::fs::write(root.join(".gitignore"), "vendor/\n").unwrap();
    std::fs::write(
        root.join("src/a.ts"),
        "hello alpha\nsecond line\nhello again\n",
    )
    .unwrap();
    std::fs::write(root.join("src/b.ts"), "hello beta\n").unwrap();
    std::fs::write(
        root.join("src/c.ts"),
        "hello one\nmid\nmid2\nmid3\nhello five\n",
    )
    .unwrap();
    std::fs::write(root.join("docs/note.md"), "hello gamma\n").unwrap();
    std::fs::write(root.join("vendor/ignored.ts"), "hello hidden\n").unwrap();
    std::fs::write(root.join("blob.bin"), b"hello \0 binary\n").unwrap();
    dir
}

#[tokio::test]
async fn grep_finds_matches_with_file_line_format() {
    let dir = grep_fixture();
    let tool = GrepTool::default();
    let out = tool
        .execute(
            json!({ "pattern": "hello", "path": dir.path().to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .expect("grep ok");

    assert!(out.contains("src/a.ts:1:hello alpha"), "got: {out}");
    assert!(out.contains("src/a.ts:3:hello again"));
    assert!(out.contains("src/b.ts:1:hello beta"));
    assert!(out.contains("docs/note.md:1:hello gamma"));
    // gitignored + binary files never appear
    assert!(!out.contains("ignored.ts"), "gitignore violated: {out}");
    assert!(!out.contains("blob.bin"), "binary leaked: {out}");
}

#[tokio::test]
async fn grep_respects_limit_and_notes_truncation() {
    let dir = grep_fixture();
    let tool = GrepTool::default();
    let out = tool
        .execute(
            json!({
                "pattern": "hello",
                "path": dir.path().to_str().unwrap(),
                "limit": 2
            }),
            &test_ctx(),
        )
        .await
        .expect("grep ok");

    let match_lines = out.lines().filter(|l| l.contains(":hello")).count();
    assert_eq!(match_lines, 2, "got: {out}");
    assert!(out.contains("[Output truncated"), "missing note: {out}");
}

#[tokio::test]
async fn grep_literal_and_ignore_case() {
    let dir = grep_fixture();
    let tool = GrepTool::default();

    // literal: a regex metachar pattern still matches literally
    let out = tool
        .execute(
            json!({
                "pattern": "a.t",
                "path": format!("{}/src/a.ts", dir.path().display()),
                "literal": true
            }),
            &test_ctx(),
        )
        .await
        .expect("literal ok");
    assert!(out.starts_with("No matches found"), "got: {out}");

    // ignoreCase
    let out = tool
        .execute(
            json!({
                "pattern": "HELLO",
                "path": dir.path().to_str().unwrap(),
                "ignoreCase": true
            }),
            &test_ctx(),
        )
        .await
        .expect("icase ok");
    assert!(out.contains(":1:hello alpha"), "got: {out}");
}

#[tokio::test]
async fn grep_context_lines_with_separator() {
    let dir = grep_fixture();
    let tool = GrepTool::default();
    let out = tool
        .execute(
            json!({
                "pattern": "hello",
                "path": format!("{}/src", dir.path().display()),
                "context": 1
            }),
            &test_ctx(),
        )
        .await
        .expect("ctx ok");

    // a.ts matches at lines 1 and 3 (one contiguous group incl. context
    // line 2); c.ts matches at lines 1 and 5 — its context groups are split
    // by an in-file `--` separator.
    assert!(out.contains("a.ts:1:hello alpha"), "got: {out}");
    assert!(out.contains("a.ts-2:second line"), "got: {out}");
    assert!(out.contains("a.ts:3:hello again"));
    assert!(out.contains("b.ts:1:hello beta"));
    assert!(out.contains("c.ts:1:hello one"), "got: {out}");
    assert!(out.contains("c.ts-2:mid"));
    assert!(out.contains("--"), "missing group separator: {out}");
    assert!(out.contains("c.ts-4:mid3"), "got: {out}");
    assert!(out.contains("c.ts:5:hello five"));

    // Without context there are no separators at all.
    let plain = tool
        .execute(
            json!({ "pattern": "hello", "path": format!("{}/src", dir.path().display()) }),
            &test_ctx(),
        )
        .await
        .expect("plain ok");
    assert!(!plain.contains("--"), "unexpected separator: {plain}");
}

#[tokio::test]
async fn grep_glob_filter_and_truncates_long_lines() {
    let dir = grep_fixture();
    let tool = GrepTool::default();

    let out = tool
        .execute(
            json!({
                "pattern": "hello",
                "path": dir.path().to_str().unwrap(),
                "glob": "*.md"
            }),
            &test_ctx(),
        )
        .await
        .expect("glob ok");
    assert!(out.contains("note.md:1:hello gamma"), "got: {out}");
    assert!(!out.contains("a.ts"), "glob leaked .ts: {out}");

    // long line truncation (500 chars)
    let long = "x".repeat(2000);
    let file = dir.path().join("src/long.ts");
    std::fs::write(&file, format!("{long}\n")).unwrap();
    let out = tool
        .execute(
            json!({ "pattern": "x+", "path": file.to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .expect("long ok");
    assert!(out.contains("… (line truncated)"), "got: {out}");
    assert!(out.chars().count() < 700, "line not truncated: {out}");
}

#[tokio::test]
async fn find_matches_glob_respecting_gitignore() {
    let dir = grep_fixture();
    let tool = FindTool::default();

    let out = tool
        .execute(
            json!({
                "pattern": "*.ts",
                "path": dir.path().to_str().unwrap()
            }),
            &test_ctx(),
        )
        .await
        .expect("find ok");

    assert!(out.contains("src/a.ts"), "got: {out}");
    assert!(out.contains("src/b.ts"));
    assert!(!out.contains("vendor/ignored.ts"), "gitignore violated: {out}");
    assert!(!out.contains("note.md"), "glob leaked .md: {out}");
}

#[tokio::test]
async fn find_limit_truncation() {
    let dir = grep_fixture();
    let tool = FindTool::default();
    let out = tool
        .execute(
            json!({
                "pattern": "*.ts",
                "path": dir.path().to_str().unwrap(),
                "limit": 1
            }),
            &test_ctx(),
        )
        .await
        .expect("find ok");
    let count = out.lines().filter(|l| l.ends_with(".ts")).count();
    assert_eq!(count, 1, "got: {out}");
}

#[tokio::test]
async fn ls_lists_dirs_first_sorted_with_sizes() {
    let dir = grep_fixture();
    let tool = ListDirTool::default();
    let out = tool
        .execute(
            json!({ "path": dir.path().to_str().unwrap() }),
            &test_ctx(),
        )
        .await
        .expect("ls ok");

    let lines: Vec<&str> = out.lines().collect();
    let first_dir_idx = lines.iter().position(|l| l.starts_with("docs/")).unwrap();
    assert!(lines.iter().take(first_dir_idx).all(|l| l.ends_with('/')), "dirs first: {out}");
    assert!(out.contains("src/"));
    assert!(out.contains("vendor/"));
    assert!(out.contains(".gitignore"));
    assert!(out.contains("blob.bin ("));
    assert!(out.contains(" bytes)"));
}

#[tokio::test]
async fn read_file_offset_limit_streaming() {
    let dir = grep_fixture();
    let tool = ReadFileTool::default();
    let out = tool
        .execute(
            json!({
                "path": format!("{}/src/a.ts", dir.path().display()),
                "offset": 2,
                "limit": 2
            }),
            &test_ctx(),
        )
        .await
        .expect("read ok");
    assert_eq!(out, "second line\nhello again");
}
