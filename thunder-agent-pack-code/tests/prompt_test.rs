//! The engineering prompt lives in the pack, not in the kernel.

use thunder_agent_pack_code::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;

#[test]
fn engineering_prompt_is_available_from_the_pack() {
    let prompt = DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;
    assert!(prompt.starts_with("# Role & Philosophy"));
    assert!(prompt.contains("<intent_analysis>"));
    assert!(prompt.contains("Ask (question / consultation)"));
    assert!(prompt.contains("Plan (planning / architecture)"));
    assert!(prompt.contains("Write / Edit (file or content operations)"));
    assert!(prompt.contains("## Runtime Guardrails"));
}
