use thunder_agent_loop::types::event::{AgentStats, FinishReason};
use thunder_agent_loop::types::message::ChatMessage;
use thunder_agent_loop::AgentRunResult;
use thunder_conversation::prelude::*;

#[tokio::test]
async fn test_sequential_pipeline_conversation() {
    let store = MemoryConversationStore::new();
    let manager = ConversationManager::new(store);

    let mut main_conv = manager
        .create_with_prompt(
            "pipeline_task_1",
            Some("Refactor Auth Module".to_string()),
            Some("You are an AI software engineering team.".to_string()),
        )
        .await
        .unwrap();

    // 1. Planner stage
    let planner_result = AgentRunResult {
        agent_id: "planner".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("1. Extract token validation. 2. Implement refresh flow.".to_string()),
        stats: AgentStats {
            total_turns: 2,
            total_duration_ms: 1200,
            total_tool_executions: 1,
            ..Default::default()
        },
        messages: vec![
            ChatMessage::user("Refactor Auth Module"),
            ChatMessage::assistant(
                Some("1. Extract token validation. 2. Implement refresh flow.".to_string()),
                None,
            ),
        ],
    };

    OrchestrationHelper::record_sequential_stage(
        &mut main_conv,
        "planner",
        "Refactor Auth Module",
        &planner_result,
    );

    assert_eq!(main_conv.stages.len(), 1);
    assert_eq!(main_conv.stages[0].role, "planner");
    assert_eq!(
        main_conv.stages[0].final_content.as_deref(),
        Some("1. Extract token validation. 2. Implement refresh flow.")
    );

    // 2. Coder stage (receives planner output as brief)
    let coder_brief = main_conv.stages[0].final_content.clone().unwrap();
    let coder_result = AgentRunResult {
        agent_id: "coder".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("Implemented TokenValidator and RefreshTokenHandler.".to_string()),
        stats: AgentStats {
            total_turns: 3,
            total_duration_ms: 2500,
            total_tool_executions: 2,
            ..Default::default()
        },
        messages: vec![
            ChatMessage::user(format!("Implement plan:\n{coder_brief}")),
            ChatMessage::assistant(
                Some("Implemented TokenValidator and RefreshTokenHandler.".to_string()),
                None,
            ),
        ],
    };

    OrchestrationHelper::record_sequential_stage(
        &mut main_conv,
        "coder",
        &coder_brief,
        &coder_result,
    );

    assert_eq!(main_conv.stages.len(), 2);
    assert_eq!(main_conv.stages[1].role, "coder");

    // Markdown export includes both stages
    let md = ConversationExporter::to_markdown(&main_conv);
    assert!(md.contains("Stage: `planner`"));
    assert!(md.contains("Stage: `coder`"));
    assert!(md.contains("Extract token validation"));
    assert!(md.contains("Implemented TokenValidator"));
}

#[tokio::test]
async fn test_parallel_branches_and_synthesis() {
    let mut parent = Conversation::new("parallel_main")
        .with_title("Code Review & Optimization")
        .with_system_prompt("Expert review council.");

    parent.add_user_message("Review PR #42 for security and performance");

    // Create 2 branches
    let branch_sec = OrchestrationHelper::create_branch(
        &parent,
        "branch_security",
        OrchestrationTopology::Parallel,
        "security_reviewer",
        "sec_agent_1",
        Some("run_100".to_string()),
    );
    assert_eq!(branch_sec.parent_id.as_deref(), Some("parallel_main"));

    let branch_perf = OrchestrationHelper::create_branch(
        &parent,
        "branch_perf",
        OrchestrationTopology::Parallel,
        "perf_reviewer",
        "perf_agent_1",
        Some("run_100".to_string()),
    );
    assert_eq!(branch_perf.parent_id.as_deref(), Some("parallel_main"));

    // Mock results
    let sec_res = AgentRunResult {
        agent_id: "sec_agent_1".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("No injection vulnerabilities found. Sanitization clean.".to_string()),
        stats: AgentStats {
            total_turns: 1,
            total_duration_ms: 800,
            total_tool_executions: 0,
            ..Default::default()
        },
        messages: vec![],
    };

    let perf_res = AgentRunResult {
        agent_id: "perf_agent_1".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("Allocation bottleneck at line 140. Recommend SmallVec.".to_string()),
        stats: AgentStats {
            total_turns: 1,
            total_duration_ms: 950,
            total_tool_executions: 1,
            ..Default::default()
        },
        messages: vec![],
    };

    let branch_results = vec![
        ("security_reviewer".to_string(), sec_res),
        ("perf_reviewer".to_string(), perf_res),
    ];

    OrchestrationHelper::merge_parallel_into_parent(&mut parent, "run_100", &branch_results);

    assert_eq!(parent.stages.len(), 2);
    let last_content = parent.last_assistant_content().unwrap();
    assert!(last_content.contains("Parallel Multi-Agent Execution Summary"));
    assert!(last_content.contains("security_reviewer"));
    assert!(last_content.contains("perf_reviewer"));
}

#[tokio::test]
async fn test_delegate_sub_conversation_recording() {
    let mut parent = Conversation::new("root_chat");
    parent.add_user_message("Please analyze dataset and generate chart");

    let sub_agent_result = AgentRunResult {
        agent_id: "chart_worker".to_string(),
        finish_reason: FinishReason::Done,
        final_content: Some("Chart generated: output/chart.png".to_string()),
        stats: AgentStats {
            total_turns: 2,
            total_duration_ms: 1500,
            total_tool_executions: 2,
            ..Default::default()
        },
        messages: vec![],
    };

    OrchestrationHelper::record_delegated_task(
        &mut parent,
        "call_delegate_001",
        "delegate_worker",
        "sub_conv_chart_123",
        &sub_agent_result,
    );

    let last_msg = parent.last_message().unwrap();
    assert_eq!(last_msg.role(), thunder_agent_loop::Role::Tool);
    if let ChatMessage::Tool {
        tool_call_id,
        content,
        name,
    } = last_msg
    {
        assert_eq!(tool_call_id, "call_delegate_001");
        assert_eq!(name.as_deref(), Some("delegate_worker"));
        assert!(content.contains("Chart generated"));
        assert!(content.contains("sub_conv_chart_123"));
    } else {
        panic!("Expected ChatMessage::Tool");
    }
}
