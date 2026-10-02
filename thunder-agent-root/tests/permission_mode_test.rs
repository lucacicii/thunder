//! Role tier × approval mode: the pure orthogonal model end to end.
//!
//! Capability ceiling is strictly governed by the role's `permission` (Read ⊂ Write ⊂ Bash).
//! Approval mode (`never`, `shell_only`, `mutations`, `always`) governs only when
//! humans are asked for confirmation.

use thunder_agent_loop::prelude::*;
use thunder_agent_root::roles::RoleRegistry;

async fn registry_with(body: &str) -> RoleRegistry {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("roles.jsonl");
    tokio::fs::write(&file, body).await.unwrap();
    RoleRegistry::load_from_sources(vec![file]).await
}

#[tokio::test]
async fn a_role_may_declare_its_mode() {
    let reg = registry_with(
        r#"{"id":"coder","permission":"bash","mode":"shell_only","persona":"编写代码"}
{"id":"auto","permission":"bash","mode":"never"}
{"id":"legacy","permission":"read"}"#,
    )
    .await;

    let coder = reg.get("coder").expect("coder role");
    assert_eq!(coder.mode, Some(ApprovalMode::ShellOnly));
    assert_eq!(coder.permission, Permission::Bash);

    assert_eq!(reg.get("auto").unwrap().mode, Some(ApprovalMode::Never));
    assert_eq!(
        reg.get("legacy").unwrap().mode,
        None,
        "mode is optional; roles without mode keep None"
    );
}

#[tokio::test]
async fn mode_aliases_are_accepted() {
    let reg = registry_with(concat!(
        r#"{"id":"a","permission":"bash","permissionMode":"accept_edits"}"#,
        "\n",
        r#"{"id":"b","permission":"bash","permission_mode":"manual"}"#,
        "\n",
        r#"{"id":"c","permission":"bash","mode":"yolo"}"#,
        "\n",
        r#"{"id":"d","permission":"bash","mode":"ask"}"#,
        "\n",
    ))
    .await;
    assert_eq!(reg.get("a").unwrap().mode, Some(ApprovalMode::ShellOnly));
    assert_eq!(reg.get("b").unwrap().mode, Some(ApprovalMode::Always));
    assert_eq!(reg.get("c").unwrap().mode, Some(ApprovalMode::Never));
    assert_eq!(reg.get("d").unwrap().mode, Some(ApprovalMode::Mutations));
}

#[tokio::test]
async fn role_and_mode_compose_without_escalating() {
    assert_eq!(
        ApprovalMode::default(),
        ApprovalMode::Never,
        "default is Never (auto) for headless/unprompted runs"
    );

    let cases = [
        // (permission, mode, expect_write_ask, expect_shell_ask)
        (Permission::Read, None, false, false),
        (Permission::Read, Some(ApprovalMode::Never), false, false),
        (
            Permission::Read,
            Some(ApprovalMode::ShellOnly),
            false,
            false,
        ),
        (
            Permission::Read,
            Some(ApprovalMode::Mutations),
            false,
            false,
        ),
        (Permission::Read, Some(ApprovalMode::Always), false, false),
        (Permission::Write, None, false, false),
        (Permission::Write, Some(ApprovalMode::Never), false, false),
        (
            Permission::Write,
            Some(ApprovalMode::ShellOnly),
            false,
            false,
        ),
        (
            Permission::Write,
            Some(ApprovalMode::Mutations),
            true,
            false,
        ),
        (Permission::Write, Some(ApprovalMode::Always), true, false),
        (Permission::Bash, None, false, false),
        (Permission::Bash, Some(ApprovalMode::Never), false, false),
        (Permission::Bash, Some(ApprovalMode::ShellOnly), false, true),
        (Permission::Bash, Some(ApprovalMode::Mutations), true, true),
        (Permission::Bash, Some(ApprovalMode::Always), true, true),
    ];

    for (permission, mode, expect_write_ask, expect_shell_ask) in cases {
        let mode = mode.unwrap_or_default();
        let label = format!("{permission:?} + {}", mode.as_str());

        let policy = SessionPolicy::new(permission, mode);
        let write_call =
            ToolCall::new_function("c1", "write_file", r#"{"path":"a","content":"b"}"#);
        let shell_call = ToolCall::new_function("c2", "bash", r#"{"command":"ls"}"#);
        let model = Caller::Model;

        let write_asks = matches!(policy.decide(&write_call, &model).await, Verdict::Ask(_));
        let shell_asks = matches!(policy.decide(&shell_call, &model).await, Verdict::Ask(_));

        assert_eq!(write_asks, expect_write_ask, "write gate for {label}");
        assert_eq!(shell_asks, expect_shell_ask, "shell gate for {label}");
    }
}

#[tokio::test]
async fn switching_roles_governs_capability_cleanly() {
    let policy = SessionPolicy::new(Permission::Bash, ApprovalMode::ShellOnly);
    let write_call = ToolCall::new_function("c1", "write_file", r#"{"path":"a","content":"b"}"#);
    let model = Caller::Model;

    // Bash tier + ShellOnly: write is allowed directly
    assert_eq!(policy.decide(&write_call, &model).await, Verdict::Allow);

    // Switch to read-only role (e.g. Plan role): writes are hard refused by tier
    policy.set_tier(Permission::Read).await;
    assert!(matches!(
        policy.decide(&write_call, &model).await,
        Verdict::Deny { .. }
    ));

    // Switch back to Bash tier: writes are allowed again
    policy.set_tier(Permission::Bash).await;
    assert_eq!(policy.decide(&write_call, &model).await, Verdict::Allow);
}

#[test]
fn modes_cycle_for_a_keyboard_toggle() {
    let mut mode = ApprovalMode::default();
    for _ in 0..ApprovalMode::ALL.len() {
        mode = mode.next();
    }
    assert_eq!(mode, ApprovalMode::default());
}
