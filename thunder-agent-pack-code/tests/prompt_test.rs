//! The engineering prompt lives in the pack, not in the kernel.

use thunder_agent_pack_code::{
    autonomous_system_prompt, RenderCapabilities, DEFAULT_AUTONOMOUS_SYSTEM_PROMPT,
};

#[test]
fn engineering_prompt_is_available_from_the_pack() {
    let prompt = DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;
    assert!(prompt.starts_with("# Role & Philosophy"));
    // The intent-classification ritual is gone: planning is a mechanism the
    // user controls, not a phase the prompt asks the model to perform. What
    // stays is the judgement — including the one-sentence rule.
    assert!(!prompt.contains("<intent_analysis>"));
    assert!(prompt.contains("fits in one sentence"));
    assert!(prompt.contains("write_plan"));
    assert!(prompt.contains("exit_plan_mode"));
}

/// The presentation rules are a contract with the shipped renderers, not style
/// advice: the prompt may only ask for forms the surface can actually draw.
#[test]
fn presentation_rules_stay_within_what_the_renderers_can_draw() {
    let prompt = DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;
    assert!(prompt.contains("# Presentation"));
    // Tables are the one visual primitive every shipped surface renders with
    // real chrome.
    assert!(prompt.contains("Markdown table"));

    // The TUI's preview renderer caps a fenced block at three rows and has no
    // diagram support, so the diagram-free variant must not mention one.
    assert!(!prompt.contains("mermaid"));
}

#[test]
fn a_diagram_capable_host_gets_the_diagram_rule() {
    let prompt = autonomous_system_prompt(RenderCapabilities { diagrams: true });
    assert!(prompt.contains("```mermaid"));
    // The rule is spliced into the layout section, never after the restraint
    // paragraph — decoration advice that follows the rules it qualifies reads
    // as one more suggestion.
    let diagram = prompt.find("```mermaid").expect("diagram rule");
    let restraint = prompt
        .find("Skip the decoration.")
        .expect("restraint paragraph");
    assert!(
        diagram < restraint,
        "diagram rule must precede the restraint paragraph"
    );
    // Everything else is identical, so a model never has to guess which rules
    // still apply when the surface changes.
    assert!(prompt.contains("# Role & Philosophy"));
    assert!(prompt.contains("Markdown table"));
}

#[test]
fn render_capabilities_default_to_the_markdown_baseline() {
    // A client that sends nothing must not be told to draw anything: absence
    // has to mean "tables and fences only", not "assume the rich renderer".
    assert_eq!(
        RenderCapabilities::default(),
        RenderCapabilities { diagrams: false }
    );
    assert_eq!(
        autonomous_system_prompt(RenderCapabilities::default()),
        DEFAULT_AUTONOMOUS_SYSTEM_PROMPT
    );
}

#[test]
fn render_capabilities_deserialize_from_a_partial_payload() {
    let caps: RenderCapabilities = serde_json::from_str(r#"{"diagrams":true}"#).unwrap();
    assert!(caps.diagrams);
    // An unknown field is ignored rather than rejected: a newer host may
    // declare a form this daemon does not know, and that must not fail a run.
    let empty: RenderCapabilities = serde_json::from_str(r#"{"math":true}"#).unwrap();
    assert!(!empty.diagrams);
}
