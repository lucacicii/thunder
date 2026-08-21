use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::pruning::strategy::ContextPruner;
use thunder_agent_loop::types::config::ContextPruningConfig;
use thunder_agent_loop::types::message::ChatMessage;

#[test]
fn test_context_pruning_sliding_window_and_tool_truncation() {
    let mut ctx = ContextBuffer::new();
    ctx.set_system_prompt("System prompt is pinned");

    // Add multiple turns of user and large tool messages
    for i in 1..=10 {
        ctx.push(ChatMessage::user(format!("User question #{}", i)));
        ctx.push(ChatMessage::assistant(
            Some(format!("Assistant thinking #{}", i)),
            None,
        ));
        ctx.push(ChatMessage::tool(
            format!("call_{}", i),
            "TOOL_LOG_OUTPUT_LONG_TEXT_".repeat(100),
            Some("bash".to_string()),
        ));
    }

    let initial_tokens = ctx.estimated_tokens();
    assert!(initial_tokens > 2000);

    let pruner = ContextPruner::new(ContextPruningConfig {
        max_context_tokens: 800,
        preserve_last_turns: 2,
        pin_system_prompt: true,
        strategy: thunder_agent_loop::types::config::PruningStrategy::Hybrid,
    });

    let res = pruner.prune(&mut ctx);
    assert!(res.pruned);
    assert!(ctx.estimated_tokens() <= 1000);
    assert_eq!(ctx.get_entry(0).unwrap().message.role(), thunder_agent_loop::types::message::Role::System);
}
