use crate::error::ConversationError;
use crate::turn::extract_turns;
use crate::types::Conversation;
use thunder_agent_loop::types::message::ChatMessage;

pub struct ConversationExporter;

impl ConversationExporter {
    /// Export conversation to structured Markdown representation.
    pub fn to_markdown(conv: &Conversation) -> String {
        let mut md = String::new();

        // Title & Header
        let title = conv.title.as_deref().unwrap_or("Untitled Conversation");
        md.push_str(&format!("# 💬 {}\n\n", title));
        md.push_str(&format!("- **ID**: `{}`\n", conv.id));
        if let Some(pid) = &conv.parent_id {
            md.push_str(&format!("- **Parent ID**: `{}`\n", pid));
        }
        md.push_str(&format!("- **Status**: `{:?}`\n", conv.status));
        md.push_str(&format!(
            "- **Messages**: {} | **Turns**: {} | **Total Tokens (Est.)**: {}\n",
            conv.messages.len(),
            conv.stats.turn_count,
            conv.stats.total_tokens
        ));

        if let Some(orch) = &conv.orchestration {
            md.push_str(&format!(
                "- **Orchestration**: topology={:?}, role={:?}, run_id={:?}\n",
                orch.topology, orch.role, orch.run_id
            ));
        }

        md.push_str("\n---\n\n");

        // Stages section if present
        if !conv.stages.is_empty() {
            md.push_str("## 🔄 Execution Stages\n\n");
            for stage in &conv.stages {
                md.push_str(&format!(
                    "### Stage: `{}` (Agent: `{}`)\n",
                    stage.role, stage.agent_id
                ));
                md.push_str(&format!("- **Brief**: {}\n", stage.task_brief));
                md.push_str(&format!(
                    "- **Outcome**: {} turns, {} tools, {}ms\n",
                    stage.turn_count, stage.tool_calls_count, stage.duration_ms
                ));
                if let Some(content) = &stage.final_content {
                    md.push_str(&format!("\n> {}\n\n", content.replace('\n', "\n> ")));
                }
            }
            md.push_str("---\n\n");
        }

        // Dialogue Turns
        md.push_str("## 📜 Dialogue Flow\n\n");
        let turns = extract_turns(&conv.messages);

        for turn in turns {
            md.push_str(&format!("### ▶ Turn {}\n\n", turn.turn_index));

            if let Some(user_msg) = &turn.user_message {
                if let Some(content) = user_msg.content_str() {
                    md.push_str(&format!("**User**:\n```\n{}\n```\n\n", content));
                }
            }

            for asst in &turn.assistant_messages {
                match asst {
                    ChatMessage::Assistant {
                        content,
                        tool_calls,
                        ..
                    } => {
                        if let Some(c) = content {
                            if !c.is_empty() {
                                md.push_str(&format!("**Assistant**:\n{}\n\n", c));
                            }
                        }
                        if let Some(calls) = tool_calls {
                            for call in calls {
                                md.push_str(&format!(
                                    "🔧 *Tool Call*: `{}` (id: `{}`)\n```json\n{}\n```\n\n",
                                    call.function.name, call.id, call.function.arguments
                                ));
                            }
                        }
                    }
                    _ => {}
                }
            }

            for tool in &turn.tool_messages {
                if let ChatMessage::Tool {
                    tool_call_id,
                    content,
                    name,
                } = tool
                {
                    let tool_label = name.as_deref().unwrap_or("tool");
                    md.push_str(&format!(
                        "⚙️ *Tool Result* (`{tool_label}` id: `{tool_call_id}`):\n```\n{}\n```\n\n",
                        content
                    ));
                }
            }
        }

        md
    }

    /// Export to formatted JSON string.
    pub fn to_json(conv: &Conversation) -> Result<String, ConversationError> {
        serde_json::to_string_pretty(conv).map_err(ConversationError::from)
    }

    /// Parse from JSON string.
    pub fn from_json(json: &str) -> Result<Conversation, ConversationError> {
        serde_json::from_str(json).map_err(ConversationError::from)
    }

    /// Convert to OpenAI-compatible ChatMessage list.
    pub fn to_openai_messages(conv: &Conversation) -> Vec<ChatMessage> {
        conv.messages.clone()
    }
}
