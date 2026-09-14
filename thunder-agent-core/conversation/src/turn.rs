use crate::types::Conversation;
use serde::{Deserialize, Serialize};
use thunder_agent_loop::core::context::ContextBuffer;
use thunder_agent_loop::types::message::{ChatMessage, Role};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub turn_index: usize,
    pub user_message: Option<ChatMessage>,
    pub assistant_messages: Vec<ChatMessage>,
    pub tool_messages: Vec<ChatMessage>,
    pub final_content: Option<String>,
    pub estimated_tokens: usize,
}

impl Turn {
    pub fn new(turn_index: usize) -> Self {
        Self {
            turn_index,
            user_message: None,
            assistant_messages: Vec::new(),
            tool_messages: Vec::new(),
            final_content: None,
            estimated_tokens: 0,
        }
    }

    pub fn tool_calls_count(&self) -> usize {
        self.assistant_messages
            .iter()
            .filter_map(|m| match m {
                ChatMessage::Assistant {
                    tool_calls: Some(calls),
                    ..
                } => Some(calls.len()),
                _ => None,
            })
            .sum()
    }
}

pub fn extract_turns(messages: &[ChatMessage]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut current_turn: Option<Turn> = None;
    let mut turn_counter = 0;

    for msg in messages {
        match msg.role() {
            Role::System => {
                // System message does not start a dialogue turn
                continue;
            }
            Role::User => {
                if let Some(mut prev) = current_turn.take() {
                    prev.final_content = extract_final_content(&prev.assistant_messages);
                    turns.push(prev);
                }
                turn_counter += 1;
                let mut turn = Turn::new(turn_counter);
                turn.estimated_tokens += ContextBuffer::calculate_message_tokens(msg);
                turn.user_message = Some(msg.clone());
                current_turn = Some(turn);
            }
            Role::Assistant => {
                if current_turn.is_none() {
                    turn_counter += 1;
                    current_turn = Some(Turn::new(turn_counter));
                }
                if let Some(turn) = current_turn.as_mut() {
                    turn.estimated_tokens += ContextBuffer::calculate_message_tokens(msg);
                    turn.assistant_messages.push(msg.clone());
                }
            }
            Role::Tool => {
                if current_turn.is_none() {
                    turn_counter += 1;
                    current_turn = Some(Turn::new(turn_counter));
                }
                if let Some(turn) = current_turn.as_mut() {
                    turn.estimated_tokens += ContextBuffer::calculate_message_tokens(msg);
                    turn.tool_messages.push(msg.clone());
                }
            }
        }
    }

    if let Some(mut last) = current_turn {
        last.final_content = extract_final_content(&last.assistant_messages);
        turns.push(last);
    }

    turns
}

fn extract_final_content(assistant_msgs: &[ChatMessage]) -> Option<String> {
    assistant_msgs.iter().rev().find_map(|m| match m {
        ChatMessage::Assistant {
            content: Some(c), ..
        } if !c.is_empty() => Some(c.clone()),
        _ => None,
    })
}

pub fn truncate_turns(conversation: &Conversation, keep_last_n: usize) -> Vec<ChatMessage> {
    if keep_last_n == 0 {
        return conversation
            .system_prompt
            .as_ref()
            .map(|p| vec![ChatMessage::system(p.clone())])
            .unwrap_or_default();
    }

    let turns = extract_turns(&conversation.messages);
    if turns.len() <= keep_last_n {
        return conversation.messages.clone();
    }

    let skip_count = turns.len() - keep_last_n;
    let retained_turns = &turns[skip_count..];

    let mut result = Vec::new();
    if let Some(sys) = &conversation.system_prompt {
        result.push(ChatMessage::system(sys.clone()));
    } else if let Some(first) = conversation.messages.first() {
        if first.role() == Role::System {
            result.push(first.clone());
        }
    }

    for turn in retained_turns {
        if let Some(user_msg) = &turn.user_message {
            result.push(user_msg.clone());
        }
        for asst_msg in &turn.assistant_messages {
            result.push(asst_msg.clone());
        }
        for tool_msg in &turn.tool_messages {
            result.push(tool_msg.clone());
        }
    }

    result
}
