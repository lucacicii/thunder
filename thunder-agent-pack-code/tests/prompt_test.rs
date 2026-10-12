//! The engineering prompt lives in the pack, not in the kernel.

use thunder_agent_pack_code::DEFAULT_AUTONOMOUS_SYSTEM_PROMPT;

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
