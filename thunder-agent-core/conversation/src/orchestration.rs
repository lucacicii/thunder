use crate::types::{Conversation, OrchestrationMeta, StageRecord};
use thunder_agent_loop::AgentRunResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestrationTopology {
    Sequential,
    Parallel,
    Delegate,
}

impl OrchestrationTopology {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Parallel => "parallel",
            Self::Delegate => "delegate",
        }
    }
}

pub struct OrchestrationHelper;

impl OrchestrationHelper {
    /// Create a branch conversation derived from a parent conversation.
    pub fn create_branch(
        parent: &Conversation,
        branch_id: impl Into<String>,
        topology: OrchestrationTopology,
        role: impl Into<String>,
        agent_id: impl Into<String>,
        run_id: Option<String>,
    ) -> Conversation {
        let role_str = role.into();
        let agent_str = agent_id.into();
        let branch_id_str = branch_id.into();

        let meta = OrchestrationMeta {
            run_id,
            topology: Some(topology.as_str().to_string()),
            role: Some(role_str.clone()),
            agent_id: Some(agent_str),
            parent_tool_call_id: None,
            auto_routed: None,
            routing_reason: None,
        };

        let mut branch = Conversation::new(branch_id_str)
            .with_parent_id(&parent.id)
            .with_title(format!("Branch [{}]: {}", topology.as_str(), role_str))
            .with_orchestration(meta);

        if let Some(sys) = &parent.system_prompt {
            branch = branch.with_system_prompt(sys.clone());
        }

        branch
    }

    /// Record a sequential stage transition into the conversation.
    pub fn record_sequential_stage(
        conv: &mut Conversation,
        role: impl Into<String>,
        brief: impl Into<String>,
        result: &AgentRunResult,
    ) {
        conv.append_stage(role, brief, result);
    }

    /// Merge multiple parallel branch results into a synthesis in the parent conversation.
    pub fn merge_parallel_into_parent(
        parent: &mut Conversation,
        run_id: &str,
        branch_results: &[(String, AgentRunResult)], // (role, result)
    ) {
        let mut synthesis = format!("### Parallel Multi-Agent Execution Summary (run_id: {run_id})\n\n");

        for (role, result) in branch_results {
            synthesis.push_str(&format!("#### Agent Role: `{role}` (id: `{}`)\n", result.agent_id));
            synthesis.push_str(&format!("- Status: `{:?}` | Turns: {} | Tool Calls: {}\n", 
                result.finish_reason, result.stats.total_turns, result.stats.total_tool_executions));
            if let Some(content) = &result.final_content {
                synthesis.push_str(&format!("\n{}\n\n", content.trim()));
            } else {
                synthesis.push_str("\n*(No final text output)*\n\n");
            }

            // Also record stage
            conv_append_stage_record(parent, role, "Parallel task", result);
        }

        parent.add_assistant_message(Some(synthesis), None);
    }

    /// Record a delegated sub-conversation execution in the parent conversation as a standard tool call/result.
    pub fn record_delegated_task(
        parent: &mut Conversation,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        sub_conv_id: impl Into<String>,
        result: &AgentRunResult,
    ) {
        let tool_id_str = tool_call_id.into();
        let sub_id_str = sub_conv_id.into();
        let name_str = tool_name.into();

        // Record tool message with sub-conversation reference
        let content = match &result.final_content {
            Some(c) => format!("{c}\n[sub_conversation_id: {sub_id_str}]"),
            None => format!("[sub_conversation_id: {sub_id_str} finished with status: {:?}]", result.finish_reason),
        };

        parent.add_tool_message(tool_id_str, content, Some(name_str));
    }
}

fn conv_append_stage_record(
    conv: &mut Conversation,
    role: &str,
    brief: &str,
    result: &AgentRunResult,
) {
    let stage_idx = conv.stages.len() + 1;
    let stage = StageRecord {
        stage_id: format!("stage_{stage_idx}_{role}"),
        role: role.to_string(),
        agent_id: result.agent_id.clone(),
        task_brief: brief.to_string(),
        final_content: result.final_content.clone(),
        turn_count: result.stats.total_turns,
        tool_calls_count: result.stats.total_tool_executions,
        duration_ms: result.stats.total_duration_ms,
        finish_reason: format!("{:?}", result.finish_reason),
        timestamp_ms: crate::types::now_ms(),
    };
    conv.stages.push(stage);
}
