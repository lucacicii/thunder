//! The mode matrix: what each mode does with each effect × scope, end to end
//! through the gate — with the two invariants that keep it honest pinned
//! separately (a rule does not leak into plan mode; no gate means allow).

use serde_json::json;
use thunder_agent_loop::types::gate::{GateVerdict, ToolGateRequest};
use thunder_agent_loop::types::policy::{Caller, ToolEffect};
use thunder_agent_loop::types::scope::Scope;
use thunder_agent_root::plugins::mode::{AllowRule, Mode, ModePlugin};

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
    thunder_agent_loop::types::gate::ToolGate::decide(plugin, &request)
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
    let shell = json!({"command": "ls"});

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

/// A remembered rule allows a later identical call *without a dialog*, and the
/// verdict says which rule paid for it.
#[test]
fn a_remembered_rule_allows_without_asking() {
    let plugin = ModePlugin::new(Mode::Ask);
    let args = json!({"command": "git status"});
    plugin.remember(AllowRule::for_call("bash", &args).unwrap());

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
    let args = json!({"command": "git status"});
    plugin.remember(AllowRule::for_call("bash", &args).unwrap());

    // The exec rule is not consulted in plan mode either: "non-Read skips the
    // rules" covers shell, so an inspection asks rather than runs silently.
    let verdict = decide(
        &plugin,
        "bash",
        json!({"command": "git status"}),
        ToolEffect::Exec,
        Scope::Unknown,
    );
    assert!(
        asks(&verdict),
        "a rule must not silence a plan-mode question: {verdict:?}"
    );

    // A write rule recorded in a freer mode is not consulted at all.
    plugin.remember(AllowRule::for_call("write_file", &json!({"path": "a.txt"})).unwrap());
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
        }
        other => panic!("expected a denial, got {other:?}"),
    }
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
