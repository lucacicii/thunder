//! Role tier × approval mode: the two-layer model end to end.
//!
//! The claim under test is the one that is easy to get wrong — **no mode can
//! grant a right the role did not already have**, and plan mode is the only
//! mode that lowers anything.

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
        r#"{"id":"plan","permission":"bash","mode":"plan","persona":"先出方案"}
{"id":"auto","permission":"bash","mode":"yolo"}
{"id":"legacy","permission":"read"}"#,
    )
    .await;

    let plan = reg.get("plan").expect("plan role");
    assert_eq!(plan.mode, Some(PermissionMode::Plan));
    assert_eq!(
        plan.permission,
        Permission::Bash,
        "the ceiling is still declared"
    );

    assert_eq!(reg.get("auto").unwrap().mode, Some(PermissionMode::Yolo));
    assert_eq!(
        reg.get("legacy").unwrap().mode,
        None,
        "mode is optional; old role files keep working"
    );
}

#[tokio::test]
async fn mode_aliases_are_accepted() {
    let reg = registry_with(concat!(
        r#"{"id":"a","permission":"bash","permissionMode":"acceptEdits"}"#,
        "\n",
        r#"{"id":"b","permission":"bash","permission_mode":"manual"}"#,
        "\n",
    ))
    .await;
    assert_eq!(
        reg.get("a").unwrap().mode,
        Some(PermissionMode::AcceptEdits)
    );
    assert_eq!(reg.get("b").unwrap().mode, Some(PermissionMode::Manual));
}

#[tokio::test]
async fn role_and_mode_compose_without_escalating() {
    // The full matrix, in two independent columns.
    //
    // `tier` is what the guard enforces. `policy asks` is what the gate *would*
    // do on its own — and note it does not consult the tier at all, because the
    // guard runs first and a read-only run never reaches the gate. That
    // separation is the point: each layer answers only its own question.
    // `None` means "the role declares no mode", which resolves to the default —
    // `Yolo`, i.e. today's no-prompt behaviour. Rows that care about prompting
    // therefore name a mode explicitly.
    assert_eq!(
        PermissionMode::default(),
        PermissionMode::Yolo,
        "the default must preserve pre-gate behaviour, or every headless host breaks on upgrade"
    );

    let cases = [
        // (role permission, role mode, effective tier, write asks?, shell asks?)
        (Permission::Read, None, Permission::Read, false, false),
        (
            Permission::Read,
            Some(PermissionMode::Ask),
            Permission::Read,
            true,
            true,
        ),
        (
            Permission::Read,
            Some(PermissionMode::Yolo),
            Permission::Read,
            false,
            false,
        ),
        (
            Permission::Read,
            Some(PermissionMode::AcceptEdits),
            Permission::Read,
            false,
            true,
        ),
        (Permission::Write, None, Permission::Write, false, false),
        (
            Permission::Write,
            Some(PermissionMode::Ask),
            Permission::Write,
            true,
            true,
        ),
        (
            Permission::Write,
            Some(PermissionMode::AcceptEdits),
            Permission::Write,
            false,
            true,
        ),
        (Permission::Bash, None, Permission::Bash, false, false),
        (
            Permission::Bash,
            Some(PermissionMode::Ask),
            Permission::Bash,
            true,
            true,
        ),
        (
            Permission::Bash,
            Some(PermissionMode::AcceptEdits),
            Permission::Bash,
            false,
            true, // accept_edits silences writes but not the shell
        ),
        (
            Permission::Bash,
            Some(PermissionMode::Manual),
            Permission::Bash,
            true,
            true,
        ),
        (
            Permission::Bash,
            Some(PermissionMode::Plan),
            Permission::Read,
            true,
            true,
        ),
        (
            Permission::Bash,
            Some(PermissionMode::Yolo),
            Permission::Bash,
            false,
            false,
        ),
    ];

    for (permission, mode, expected_tier, expect_write_ask, expect_shell_ask) in cases {
        let mode = mode.unwrap_or_default();
        let label = format!("{permission:?} + {}", mode.as_str());

        assert_eq!(
            mode.effective(permission),
            expected_tier,
            "tier for {label}"
        );
        // The load-bearing assertion: nothing above ever produces a tier the
        // role did not already grant.
        assert!(expected_tier <= permission, "{label} escalated the ceiling");

        // The tier is the *unclipped* role tier: the policy applies the mode's
        // ceiling itself, so `plan` refuses writes even when handed `bash`.
        let policy = SessionPolicy::new(permission, mode);
        let write_call =
            ToolCall::new_function("c1", "write_file", r#"{"path":"a","content":"b"}"#);
        let shell_call = ToolCall::new_function("c2", "bash", r#"{"command":"ls"}"#);
        let model = Caller::Model;

        // Only a call the ceiling still permits can reach a prompt at all, so
        // the expected prompting is gated on the tier.
        let write_allowed = expected_tier >= ToolEffect::Write.required_tier();
        let shell_allowed = expected_tier >= ToolEffect::Exec.required_tier();
        let write_asks = matches!(policy.decide(&write_call, &model).await, Verdict::Ask(_));
        let shell_asks = matches!(policy.decide(&shell_call, &model).await, Verdict::Ask(_));
        // Under `plan` the ceiling is read-only, so nothing is ever *asked* —
        // it is refused. That is the "narrow, don't negotiate" property, and it
        // is why the expected prompting is gated on the tier.
        assert_eq!(
            write_asks,
            expect_write_ask && write_allowed,
            "write gate for {label}"
        );
        assert_eq!(
            shell_asks,
            expect_shell_ask && shell_allowed,
            "shell gate for {label}"
        );
    }
}

#[tokio::test]
async fn plan_mode_removes_writes_by_clipping_the_tier_not_by_asking() {
    // Plan mode is not "ask about writes" — writes are not available. The tier
    // is clipped to read, the guard refuses, and no dialog is ever raised.
    let mode = PermissionMode::Plan;
    assert_eq!(mode.effective(Permission::Bash), Permission::Read);
    assert_eq!(mode.effective(Permission::Write), Permission::Read);

    // Handed the *widest* tier, the policy still refuses: it applies the mode's
    // ceiling itself, so a caller cannot forget to clip it. Before that, the
    // host clipped on the way in and a policy built directly with `Bash` +
    // `Plan` restricted nothing — the most dangerous reading of "plan" there
    // was, and this assertion could not have been written.
    let policy = SessionPolicy::new(Permission::Bash, mode);
    let write_call = ToolCall::new_function("c1", "write_file", r#"{"path":"a","content":"b"}"#);
    let shell_call = ToolCall::new_function("c2", "bash", r#"{"command":"ls"}"#);
    let model = Caller::Model;

    for (tool, decision) in [
        ("write_file", policy.decide(&write_call, &model).await),
        ("bash", policy.decide(&shell_call, &model).await),
    ] {
        match decision {
            Verdict::Deny { reason } => {
                assert!(
                    reason.contains("read-only"),
                    "{tool}: the reason should name the ceiling, got: {reason}"
                );
            }
            other => panic!("{tool}: plan mode must refuse outright, never negotiate: {other:?}"),
        }
    }

    // And a read is still fine, or the mode would be useless.
    let read_call = ToolCall::new_function("c3", "read_file", r#"{"path":"a"}"#);
    assert_eq!(policy.decide(&read_call, &model).await, Verdict::Allow);
}

/// Switching out of `plan` must restore the role's own tier. Without keeping the
/// unclipped tier alongside, a `plan` → `ask` switch would strand the run at
/// read-only.
#[tokio::test]
async fn leaving_plan_mode_restores_the_role_tier() {
    let policy = SessionPolicy::new(Permission::Bash, PermissionMode::Plan);
    let write_call = ToolCall::new_function("c1", "write_file", r#"{"path":"a","content":"b"}"#);
    let model = Caller::Model;
    assert!(matches!(
        policy.decide(&write_call, &model).await,
        Verdict::Deny { .. }
    ));

    policy.set_mode(PermissionMode::Ask).await;
    assert!(
        matches!(policy.decide(&write_call, &model).await, Verdict::Ask(_)),
        "ask mode should negotiate again, not stay stuck read-only"
    );
    assert_eq!(policy.tier().await, Permission::Bash);
}

#[test]
fn modes_cycle_for_a_keyboard_toggle() {
    // The TUI's Shift+Tab equivalent: a closed cycle with no dead ends.
    let mut mode = PermissionMode::default();
    for _ in 0..PermissionMode::ALL.len() {
        mode = mode.next();
    }
    assert_eq!(mode, PermissionMode::default());
}
