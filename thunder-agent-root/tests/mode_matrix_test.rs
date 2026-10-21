//! The mode matrix: what each mode does with each effect × scope, end to end
//! through the gate — with the two invariants that keep it honest pinned
//! separately (a rule does not leak into plan mode; no gate means allow).

use serde_json::json;
use std::path::PathBuf;
use thunder_agent_policy::gate::{ApprovalAnswer, GateVerdict, ToolGate, ToolGateRequest};
use thunder_agent_root::plugins::mode::{
    AllowRule, Mode, ModePlugin, EXIT_PLAN_MODE_TOOL, WRITE_PLAN_TOOL,
};
use thunder_agent_types::policy::{Caller, ToolEffect};
use thunder_agent_types::scope::Scope;

fn decide(
    plugin: &ModePlugin,
    tool: &str,
    args: serde_json::Value,
    effect: ToolEffect,
    scope: Scope,
) -> GateVerdict {
    let request = ToolGateRequest {
        tool,
        arguments: &args,
        effect,
        scope,
        caller: Caller::Model,
        tool_call_id: "c1",
    };
    thunder_agent_policy::gate::ToolGate::decide(plugin, &request)
}

fn asks(verdict: &GateVerdict) -> bool {
    matches!(verdict, GateVerdict::Ask(_))
}

fn denies(verdict: &GateVerdict) -> bool {
    matches!(verdict, GateVerdict::Deny { .. })
}

/// One cell per row of the matrix in the module docs. The rows are (mode,
/// effect, scope) → ask? deny? allow implied.
#[test]
fn the_matrix_is_what_the_docs_say() {
    let write = json!({"path": "a.txt", "content": "x"});
    let read = json!({"path": "a.txt"});
    // Shell that *does* something. A read-only command is the one shell call
    // plan mode reads through (see `a_read_only_command_runs_in_plan_mode`), so
    // `ls` would be the wrong fixture for a row that expects a question.
    let shell = json!({"command": "cargo test"});

    let cases: Vec<(
        &str,
        &str,
        serde_json::Value,
        ToolEffect,
        Scope,
        fn(&GateVerdict) -> bool,
    )> = vec![
        // plan: reads run; everything else is refused, wherever it lands.
        (
            "plan",
            "read_file",
            read.clone(),
            ToolEffect::Read,
            Scope::InScope,
            |v| !asks(v) && !denies(v),
        ),
        (
            "plan",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::InScope,
            denies,
        ),
        (
            "plan",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::OutOfScope,
            denies,
        ),
        (
            "plan",
            "bash",
            shell.clone(),
            ToolEffect::Exec,
            Scope::Unknown,
            asks,
        ),
        // ask: reads run; changes ask, wherever they land.
        (
            "ask",
            "read_file",
            read.clone(),
            ToolEffect::Read,
            Scope::InScope,
            |v| !asks(v) && !denies(v),
        ),
        (
            "ask",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::InScope,
            asks,
        ),
        (
            "ask",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::OutOfScope,
            asks,
        ),
        (
            "ask",
            "bash",
            shell.clone(),
            ToolEffect::Exec,
            Scope::Unknown,
            asks,
        ),
        // acceptEdits: the working area is the whole point; the rest asks.
        (
            "acceptEdits",
            "read_file",
            read.clone(),
            ToolEffect::Read,
            Scope::InScope,
            |v| !asks(v) && !denies(v),
        ),
        (
            "acceptEdits",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::InScope,
            |v| !asks(v) && !denies(v),
        ),
        (
            "acceptEdits",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::OutOfScope,
            asks,
        ),
        (
            "acceptEdits",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::Unknown,
            asks,
        ),
        (
            "acceptEdits",
            "bash",
            shell.clone(),
            ToolEffect::Exec,
            Scope::Unknown,
            asks,
        ),
        // yolo: nothing asks, nothing is denied.
        (
            "yolo",
            "write_file",
            write.clone(),
            ToolEffect::Write,
            Scope::OutOfScope,
            |v| !asks(v) && !denies(v),
        ),
        (
            "yolo",
            "bash",
            shell,
            ToolEffect::Exec,
            Scope::Unknown,
            |v| !asks(v) && !denies(v),
        ),
    ];

    for (mode_name, tool, args, effect, scope, holds) in cases {
        let mode = Mode::parse(mode_name).expect("mode name");
        let plugin = ModePlugin::new(mode);
        let verdict = decide(&plugin, tool, args, effect, scope);
        assert!(
            holds(&verdict),
            "{mode_name} × {tool} × {effect:?} × {scope:?} produced {verdict:?}"
        );
    }
}

/// Plan mode reads a command through and runs it if it provably only reads —
/// the alternative was a dialog asking "may I look?", which is the question the
/// user answered by choosing plan mode in the first place.
///
/// The test is deliberately two-sided: what runs unattended, and what still
/// asks. Only the second list keeps the first honest.
#[test]
fn a_read_only_command_runs_in_plan_mode() {
    let plugin = ModePlugin::new(Mode::Plan);

    for command in [
        "ls -la",
        "cat README.md",
        "grep -rn foo src/",
        "git status --short",
        "find . -name '*.rs'",
        // The case this exists for: a pasted clipboard image lives outside the
        // workspace, and asking the model to shell out to it is how the user
        // ended up answering a dialog about `ls`.
        "file '/var/folders/jg/xx/T/agent-resume-clipboard-3f1a.png'",
    ] {
        let args = json!({ "command": command });
        let verdict = decide(&plugin, "bash", args, ToolEffect::Exec, Scope::Unknown);
        assert!(
            !asks(&verdict) && !denies(&verdict),
            "`{command}` should run in plan mode: {verdict:?}"
        );
    }

    for command in [
        "cargo test",
        "rm -rf build",
        "git commit -m x",
        "ls; rm -rf /",
        "echo hi > out.txt",
        "cat ~/.ssh/id_rsa",
    ] {
        let args = json!({ "command": command });
        let verdict = decide(&plugin, "bash", args, ToolEffect::Exec, Scope::Unknown);
        assert!(
            asks(&verdict),
            "`{command}` must still ask in plan mode: {verdict:?}"
        );
    }
}

/// The other modes are unchanged by that: they ask about shell whoever reads
/// the command, because there the question is about running something rather
/// than about planning.
#[test]
fn read_only_shell_still_asks_outside_plan_mode() {
    for mode in [Mode::Ask, Mode::AcceptEdits] {
        let plugin = ModePlugin::new(mode);
        let verdict = decide(
            &plugin,
            "bash",
            json!({ "command": "ls -la" }),
            ToolEffect::Exec,
            Scope::Unknown,
        );
        assert!(asks(&verdict), "{mode:?} should ask: {verdict:?}");
    }
}

/// The answer the kernel hands back after "Always allow", without a dialog in
/// the way — which is all `record_answer` ever needs.
fn answer_always(plugin: &ModePlugin, tool: &str, args: &serde_json::Value) {
    let request = ToolGateRequest {
        tool,
        arguments: args,
        effect: ToolEffect::of(tool),
        scope: Scope::Unknown,
        caller: Caller::Model,
        tool_call_id: "c1",
    };
    ToolGate::record_answer(plugin, &request, &ApprovalAnswer::AllowAlways);
}

/// "Always allow" that means *always*: the rule is written out, and a plugin
/// built for the next session reads it back and stops asking.
///
/// This is the whole point of the file — the complaint that produced it was a
/// user answering the same dialog twice — so it is pinned end to end rather than
/// at the store's own level.
#[test]
fn an_answered_rule_outlives_the_session_that_agreed_to_it() {
    let dir = std::env::temp_dir().join(format!("thunder-mode-rules-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("permissions.json");

    // Session one: the user is asked, and answers "Always allow".
    let first = ModePlugin::new(Mode::Ask);
    first.load_rules_from(path.clone());
    let approved = json!({ "command": "cargo test" });
    assert!(asks(&decide(
        &first,
        "bash",
        approved.clone(),
        ToolEffect::Exec,
        Scope::Unknown
    )));
    answer_always(&first, "bash", &approved);

    // Session two: a fresh plugin over the same file, and no question.
    let second = ModePlugin::new(Mode::Ask);
    second.load_rules_from(path.clone());
    let verdict = decide(
        &second,
        "bash",
        json!({ "command": "cargo test --lib" }),
        ToolEffect::Exec,
        Scope::Unknown,
    );
    assert!(
        matches!(verdict, GateVerdict::AllowByRule { .. }),
        "a remembered rule should pay in the next session: {verdict:?}"
    );

    // And the file is where the next session looks, not a copy of it.
    assert!(path.exists(), "the rule was written to {}", path.display());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The spelling a user approved and the spelling the model writes next are not
/// always the same one, and a rule that only paid for one of them read as
/// "always allow does not work".
#[test]
fn a_rule_pays_for_another_spelling_of_the_same_target() {
    let plugin = ModePlugin::new(Mode::Ask);
    plugin.bind_session(Some(PathBuf::from("/ws")), "s1");

    let approved = json!({ "path": "./a.txt", "content": "x" });
    answer_always(&plugin, "write_file", &approved);

    // Absolute instead of relative, and no dot-slash: the same file.
    let verdict = decide(
        &plugin,
        "write_file",
        json!({ "path": "/ws/a.txt", "content": "x" }),
        ToolEffect::Write,
        Scope::InScope,
    );
    assert!(
        matches!(verdict, GateVerdict::AllowByRule { .. }),
        "the same file, written another way: {verdict:?}"
    );

    // A sibling that merely shares a prefix is *not* the same file.
    let verdict = decide(
        &plugin,
        "write_file",
        json!({ "path": "/ws/a.txt.bak", "content": "x" }),
        ToolEffect::Write,
        Scope::InScope,
    );
    assert!(asks(&verdict), "a sibling must still ask: {verdict:?}");

    // Nor is a path that climbs out of the directory the rule named.
    let verdict = decide(
        &plugin,
        "write_file",
        json!({ "path": "/ws/../etc/passwd", "content": "x" }),
        ToolEffect::Write,
        Scope::InScope,
    );
    assert!(
        asks(&verdict),
        "`..` must not inherit the grant: {verdict:?}"
    );
}

/// Two sessions share one file, and neither may drop the other's grant.
///
/// Each plugin holds the list it read when it was created, so "write my list"
/// would make the last click decide the whole file — a rule the user agreed to
/// would vanish with nothing to show for it. The write merges instead, and this
/// is the scenario that fails without it.
#[test]
fn two_sessions_do_not_erase_each_other() {
    let dir = std::env::temp_dir().join(format!("thunder-mode-shared-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("permissions.json");

    // Both sessions start before either has agreed to anything.
    let panel = ModePlugin::new(Mode::Ask);
    panel.load_rules_from(path.clone());
    let tui = ModePlugin::new(Mode::Ask);
    tui.load_rules_from(path.clone());

    answer_always(&panel, "bash", &json!({ "command": "cargo test" }));
    answer_always(&tui, "bash", &json!({ "command": "git status" }));

    // Session three sees both, not whichever wrote last.
    let next = ModePlugin::new(Mode::Ask);
    next.load_rules_from(path.clone());
    let remembered: Vec<String> = next
        .rules()
        .iter()
        .filter_map(|rule| rule.arg_prefix.clone())
        .collect();
    assert!(
        remembered.contains(&"cargo test".to_string())
            && remembered.contains(&"git status".to_string()),
        "both grants should survive: {remembered:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A remembered rule allows a later identical call *without a dialog*, and the
/// verdict says which rule paid for it.
#[test]
fn a_remembered_rule_allows_without_asking() {
    let plugin = ModePlugin::new(Mode::Ask);
    let args = json!({"command": "git status"});
    plugin.remember(AllowRule::for_call("bash", &args, None).unwrap());

    let verdict = decide(
        &plugin,
        "bash",
        json!({"command": "git status --short"}),
        ToolEffect::Exec,
        Scope::Unknown,
    );
    assert!(
        matches!(verdict, GateVerdict::AllowByRule { .. }),
        "the rule should pay: {verdict:?}"
    );
}

/// The invariant Claude Code states and plan mode exists to enforce: no rule
/// recorded in a freer mode is a way to write while planning.
#[test]
fn a_rule_does_not_leak_into_plan_mode() {
    let plugin = ModePlugin::new(Mode::Plan);
    // A command that *does* something, so the only thing that could silence the
    // question is the rule — a read-only command needs no rule to run, and would
    // pass this assertion for the wrong reason.
    let args = json!({"command": "git commit -m x"});
    plugin.remember(AllowRule::for_call("bash", &args, None).unwrap());

    // The exec rule is not consulted in plan mode either: "non-Read skips the
    // rules" covers shell, so a command that changes something asks rather than
    // running silently on the authority of a grant given in a freer mode.
    let verdict = decide(
        &plugin,
        "bash",
        json!({"command": "git commit -m x"}),
        ToolEffect::Exec,
        Scope::Unknown,
    );
    assert!(
        asks(&verdict),
        "a rule must not silence a plan-mode question: {verdict:?}"
    );

    // A write rule recorded in a freer mode is not consulted at all.
    plugin.remember(AllowRule::for_call("write_file", &json!({"path": "a.txt"}), None).unwrap());
    let verdict = decide(
        &plugin,
        "write_file",
        json!({"path": "a.txt", "content": "x"}),
        ToolEffect::Write,
        Scope::InScope,
    );
    assert!(
        denies(&verdict),
        "plan mode must not write, rule or no rule: {verdict:?}"
    );
}

/// Plan's refusal names the way out, or the model will retry the write instead
/// of leaving plan mode.
#[test]
fn plan_denials_explain_how_to_leave_plan_mode() {
    let plugin = ModePlugin::new(Mode::Plan);
    let verdict = decide(
        &plugin,
        "write_file",
        json!({"path": "a.txt", "content": "x"}),
        ToolEffect::Write,
        Scope::InScope,
    );
    match verdict {
        GateVerdict::Deny { reason } => {
            assert!(reason.contains("plan mode"), "got: {reason}");
            // Every tool is registered, so the model *does* see `write_file`
            // here. Without the exit named, the refusal reads as "try again
            // differently" and the model retries the same call.
            assert!(
                reason.contains("write_plan"),
                "must name the write: {reason}"
            );
            assert!(
                reason.contains("exit_plan_mode"),
                "must name the exit: {reason}"
            );
        }
        other => panic!("expected a denial, got {other:?}"),
    }
}

/// The plan tools are the door out of plan mode, so they run there — but by
/// *name*, never because a tool declared an effect about itself.
#[test]
fn the_plan_tools_are_exempt_where_a_self_declared_effect_would_not_be() {
    let plugin = ModePlugin::new(Mode::Plan);

    // The one write plan mode allows, and the exit itself.
    for tool in [WRITE_PLAN_TOOL, EXIT_PLAN_MODE_TOOL] {
        let verdict = decide(
            &plugin,
            tool,
            json!({}),
            ToolEffect::PlanArtifact,
            Scope::Unknown,
        );
        assert!(
            !asks(&verdict) && !denies(&verdict),
            "`{tool}` must run in plan mode: {verdict:?}"
        );
    }

    // A tool that claims the plan effect without being one is denied like any
    // other write: the exemption is keyed on the name for exactly this reason.
    let verdict = decide(
        &plugin,
        "sneaky_writer",
        json!({}),
        ToolEffect::PlanArtifact,
        Scope::InScope,
    );
    assert!(
        denies(&verdict),
        "a self-declared plan effect must not buy an exemption: {verdict:?}"
    );
}

/// The names users and configs write — including every pre-0.4 name, and the
/// one word 0.3 parsed wrongly.
#[test]
fn parse_accepts_the_names_that_exist_in_the_wild() {
    assert_eq!(Mode::parse("plan"), Some(Mode::Plan));
    assert_eq!(Mode::parse("ask"), Some(Mode::Ask));
    assert_eq!(Mode::parse("acceptEdits"), Some(Mode::AcceptEdits));
    assert_eq!(Mode::parse("accept-edits"), Some(Mode::AcceptEdits));
    assert_eq!(Mode::parse("yolo"), Some(Mode::Yolo));
    // The old kernel names still mean what their authors meant.
    assert_eq!(Mode::parse("never"), Some(Mode::Yolo));
    assert_eq!(Mode::parse("mutations"), Some(Mode::Ask));
    assert_eq!(Mode::parse("writes"), Some(Mode::Ask));
    assert_eq!(Mode::parse("always"), Some(Mode::Ask));
    assert_eq!(Mode::parse("manual"), Some(Mode::Ask));
    assert_eq!(Mode::parse("shell_only"), Some(Mode::AcceptEdits));
    assert_eq!(Mode::parse("bypassPermissions"), Some(Mode::Yolo));
    assert_eq!(Mode::parse("nonsense"), None);
}

#[test]
fn modes_cycle_for_a_keyboard_toggle() {
    let mut mode = Mode::default();
    for _ in 0..Mode::ALL.len() {
        mode = mode.next();
    }
    assert_eq!(mode, Mode::default());
}

/// Flipping the mode takes effect on the very next call — the kernel asks the
/// gate each time, so this is the plugin's promise to keep its state live.
#[test]
fn flipping_the_mode_applies_to_the_next_call() {
    let plugin = ModePlugin::new(Mode::Yolo);
    let args = json!({"path": "a.txt", "content": "x"});
    assert!(!asks(&decide(
        &plugin,
        "write_file",
        args.clone(),
        ToolEffect::Write,
        Scope::InScope
    )));

    plugin.set_mode(Mode::Ask);
    assert!(asks(&decide(
        &plugin,
        "write_file",
        args,
        ToolEffect::Write,
        Scope::InScope
    )));
}

/// The announcing setter is the same flip plus a report of what it replaced.
///
/// "Nothing changed" must be distinguishable from "changed", because the host
/// re-applies the run's mode at every run start: without this rule every turn
/// would look like a mode change and a panel would learn to ignore the event.
#[test]
fn announcing_returns_the_previous_mode_only_when_it_changed() {
    let plugin = ModePlugin::new(Mode::Yolo);

    assert_eq!(
        plugin.set_mode_announcing(Mode::Plan),
        Some(Mode::Yolo),
        "a real flip reports what it replaced"
    );
    assert_eq!(plugin.mode(), Mode::Plan);

    assert_eq!(
        plugin.set_mode_announcing(Mode::Plan),
        None,
        "re-applying the same mode is not a change"
    );
    assert_eq!(plugin.mode(), Mode::Plan);

    // And the report is the *previous* value, not the new one.
    assert_eq!(
        plugin.set_mode_announcing(Mode::AcceptEdits),
        Some(Mode::Plan)
    );
}
