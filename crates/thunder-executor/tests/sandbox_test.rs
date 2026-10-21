use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use tempfile::tempdir;
use thunder_executor::{execute_isolated_script, McpToolProvider};
use thunder_protocol::{ExecutionContext, ToolExecutionError};

#[tokio::test]
async fn script_receives_stdin_json_and_env_vars() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("echo_ctx.sh");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail
payload=$(cat)
echo "CWD=${THUNDER_CWD}|SESSION=${THUNDER_SESSION_ID}|PAYLOAD=${payload}"
"#;
    tokio::fs::write(&script_path, script).await.unwrap();
    let mut perms = tokio::fs::metadata(&script_path)
        .await
        .unwrap()
        .permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&script_path, perms)
        .await
        .unwrap();

    let ctx = ExecutionContext::new("sess-101", dir.path(), Duration::from_secs(5));
    let out = execute_isolated_script(&script_path, json!({"base_branch": "main"}), &ctx)
        .await
        .expect("script succeeds");

    assert!(out.contains("SESSION=sess-101"));
    assert!(out.contains(&format!("CWD={}", dir.path().display())));
    assert!(out.contains(r#"PAYLOAD={"base_branch":"main"}"#));
}

#[tokio::test]
async fn script_flood_over_64kb_does_not_deadlock_and_spills_to_tmp() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("flood.sh");
    // Write > 200 KB to stdout (exceeds OS 64KB pipe buffer) to verify no deadlock occurs.
    let script = r#"#!/usr/bin/env bash
python3 -c 'import sys; sys.stdout.write("X" * 200000)'
"#;
    tokio::fs::write(&script_path, script).await.unwrap();
    let mut perms = tokio::fs::metadata(&script_path)
        .await
        .unwrap()
        .permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&script_path, perms)
        .await
        .unwrap();

    let ctx = ExecutionContext::new("sess-flood", dir.path(), Duration::from_secs(10));
    let out = execute_isolated_script(&script_path, json!({}), &ctx)
        .await
        .expect("flood script must not deadlock");

    assert!(out.contains("[Output truncated: Exceeded 65,536 bytes limit. Full log written to "));
}

#[tokio::test]
async fn script_nonzero_exit_strips_ansi_codes() {
    let dir = tempdir().unwrap();
    let script_path = dir.path().join("fail_ansi.sh");
    let script = r#"#!/usr/bin/env bash
printf "\033[31;1mFATAL:\033[0m merge conflict on \033[33mmain\033[0m\n" >&2
exit 42
"#;
    tokio::fs::write(&script_path, script).await.unwrap();
    let mut perms = tokio::fs::metadata(&script_path)
        .await
        .unwrap()
        .permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&script_path, perms)
        .await
        .unwrap();

    let ctx = ExecutionContext::new("sess-ansi", dir.path(), Duration::from_secs(5));
    let err = execute_isolated_script(&script_path, json!({}), &ctx)
        .await
        .expect_err("must fail");

    match err {
        ToolExecutionError::NonZeroExit { code, stderr } => {
            assert_eq!(code, 42);
            assert_eq!(stderr.trim(), "FATAL: merge conflict on main");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn script_timeout_kills_entire_process_group_including_background_children() {
    let dir = tempdir().unwrap();
    let marker = dir.path().join("child_pid.txt");
    let script_path = dir.path().join("hang_tree.sh");
    let script = format!(
        r#"#!/usr/bin/env bash
sleep 60 &
child_pid=$!
echo -n "$child_pid" > "{}"
wait $child_pid
"#,
        marker.display()
    );
    tokio::fs::write(&script_path, script).await.unwrap();
    let mut perms = tokio::fs::metadata(&script_path)
        .await
        .unwrap()
        .permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&script_path, perms)
        .await
        .unwrap();

    let ctx = ExecutionContext::new("sess-timeout", dir.path(), Duration::from_millis(350));
    let err = execute_isolated_script(&script_path, json!({}), &ctx)
        .await
        .expect_err("must time out");

    assert!(matches!(err, ToolExecutionError::Timeout));

    // Verify grandchild `sleep 60` process was also killed by process-group SIGKILL.
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Ok(pid_str) = tokio::fs::read_to_string(&marker).await {
        if let Ok(grandchild_pid) = pid_str.trim().parse::<i32>() {
            let alive = unsafe { libc::kill(grandchild_pid, 0) == 0 };
            assert!(
                !alive,
                "grandchild process {grandchild_pid} must be killed with process group"
            );
        }
    }
}

#[test]
fn third_party_mcp_schema_rewrites_relative_paths_only_for_path_fields() {
    let ws = std::path::Path::new("/workspace/demo");
    let schema = json!({
        "type": "object",
        "properties": {
            "path": { "type": "string" },
            "branch": { "type": "string" },
            "custom_file": { "type": "string", "format": "file-path" }
        }
    });
    let args = json!({
        "path": "src/main.rs",
        "branch": "feature/login",
        "custom_file": "configs/app.toml"
    });

    let rewritten = McpToolProvider::resolve_relative_paths_by_schema(&args, &schema, ws);
    assert_eq!(rewritten["path"], "/workspace/demo/src/main.rs");
    assert_eq!(rewritten["branch"], "feature/login");
    assert_eq!(rewritten["custom_file"], "/workspace/demo/configs/app.toml");
}
