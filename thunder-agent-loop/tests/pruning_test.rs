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
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            summarizer_model: None,
            summarizer_max_tokens: 4096,
        max_context_tokens: 800,
        tool_eviction_threshold_tokens: 600,
        preserve_last_turns: 2,
        pin_system_prompt: true,
        strategy: thunder_agent_loop::types::config::PruningStrategy::Hybrid,
    });

    let res = pruner.prune(&mut ctx);
    assert!(res.pruned);
    assert!(ctx.estimated_tokens() <= 1000);
    assert_eq!(ctx.get_entry(0).unwrap().message.role(), thunder_agent_loop::types::message::Role::System);
}

#[test]
fn test_tool_output_eviction_decoupled_from_huge_context_window() {
    let mut ctx = ContextBuffer::new();
    ctx.set_system_prompt("System prompt is pinned");

    // Add 10 turns of user dialogues and large tool outputs (representing bulky intermediate logs)
    for i in 1..=10 {
        ctx.push(ChatMessage::user(format!("User question #{}", i)));
        ctx.push(ChatMessage::assistant(
            Some(format!("Assistant analysis #{}", i)),
            None,
        ));
        ctx.push(ChatMessage::tool(
            format!("call_{}", i),
            format!("TOOL_STDOUT_SPAM_LINE_{}_", i).repeat(80),
            Some("bash".to_string()),
        ));
    }

    let initial_tokens = ctx.estimated_tokens();
    assert!(initial_tokens > 2000);
    let initial_msg_count = ctx.len();

    // Model has a 1,000,000 token context window, so hard limit is huge (1M)
    // But tool eviction threshold is 1000 tokens!
    let pruner = ContextPruner::new(ContextPruningConfig {
            reserve_tokens: 16_384,
            keep_recent_tokens: 20_000,
            summarizer_model: None,
            summarizer_max_tokens: 4096,
        max_context_tokens: 1_000_000,
        tool_eviction_threshold_tokens: 1_000,
        preserve_last_turns: 2,
        pin_system_prompt: true,
        strategy: thunder_agent_loop::types::config::PruningStrategy::Hybrid,
    });

    let res = pruner.prune(&mut ctx);
    assert!(res.pruned);
    assert!(res.tool_outputs_truncated > 0);
    // Crucial: NOT A SINGLE MESSAGE WAS DELETED! The 1M conversation tree is 100% preserved!
    assert_eq!(res.messages_removed, 0);
    assert_eq!(ctx.len(), initial_msg_count);

    // Old tool outputs (> preserve_last_turns) were trimmed
    if let ChatMessage::Tool { content, .. } = &ctx.get_entry(3).unwrap().message {
        assert!(content.contains("[... Older tool output trimmed by ContextPruner ...]"));
    } else {
        panic!("Expected tool message at index 3");
    }

    // Recent tool outputs (last 2 turns) were NOT trimmed and remain full-fidelity
    let last_tool_idx = ctx.len() - 1;
    if let ChatMessage::Tool { content, .. } = &ctx.get_entry(last_tool_idx).unwrap().message {
        assert!(!content.contains("Older tool output trimmed"));
    } else {
        panic!("Expected tool message at last index");
    }
}
