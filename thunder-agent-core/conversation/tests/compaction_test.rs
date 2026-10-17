//! `/compact` must leave a transcript a provider will accept: no orphaned
//! tool result, no fabricated summary the model would reason from.

use thunder_agent_loop::types::message::ChatMessage;
use thunder_conversation::prelude::*;

fn conversation() -> Conversation {
    let mut conv = Conversation::new("compact_test").with_system_prompt("SYSTEM PROMPT");
    for turn in 0..3 {
        conv.add_user_message(format!("question {turn}"));
        conv.add_assistant_message(Some(format!("answer {turn}")), None);
    }
    conv
}

#[test]
fn compaction_keeps_the_system_prompt_first() {
    let mut conv = conversation();
    let (before, after) = conv.compact_history(2);

    assert_eq!(before, 7, "system + three user/assistant pairs");
    assert!(after < before);
    assert_eq!(
        conv.messages[0].content_str(),
        Some("SYSTEM PROMPT"),
        "the pinned prompt stays at index 0"
    );
}

#[test]
fn compaction_does_not_leave_an_orphaned_tool_result() {
    let mut conv = Conversation::new("orphan").with_system_prompt("SYSTEM");
    conv.add_user_message("read it".to_string());
    conv.add_assistant_message(
        None,
        Some(vec![
            thunder_agent_loop::types::message::ToolCall::new_function("call_1", "read_file", "{}"),
        ]),
    );
    conv.messages.push(ChatMessage::tool(
        "call_1",
        "file contents",
        Some("read_file".to_string()),
    ));
    conv.add_user_message("thanks".to_string());
    conv.add_assistant_message(Some("done".to_string()), None);

    // A window that would start exactly on the tool result.
    conv.compact_history(3);

    assert!(
        !matches!(conv.messages[2], ChatMessage::Tool { .. }),
        "the kept window must not begin with a tool result: {:?}",
        conv.messages
    );
}

#[test]
fn compaction_says_what_it_removed_instead_of_inventing_a_summary() {
    let mut conv = conversation();
    conv.compact_history(2);

    let notice = conv.messages[1].content_str().unwrap_or_default();
    assert!(
        notice.contains("Earlier context trimmed"),
        "the notice must describe the trim: {notice}"
    );
    assert!(
        !notice.contains("dialogue turns discussed"),
        "the old fabricated summary must be gone: {notice}"
    );
}

#[test]
fn a_zero_keep_count_does_not_panic() {
    let mut conv = conversation();
    let (before, after) = conv.compact_history(0);

    assert_eq!(before, 7);
    // The window is clamped to the last message rather than to an index past
    // the end, so `0` trims to system + notice + that message.
    assert_eq!(after, 3);
    assert_eq!(conv.messages[0].content_str(), Some("SYSTEM PROMPT"));
    assert_eq!(conv.messages.last().unwrap().content_str(), Some("answer 2"));
}

#[test]
fn a_checkpoint_summary_is_not_silently_dropped() {
    // The in-loop checkpoint compaction leaves [system, system(summary), ...].
    let mut conv = Conversation::new("checkpoint").with_system_prompt("SYSTEM");
    conv.messages
        .push(ChatMessage::system("CHECKPOINT SUMMARY OF EARLIER WORK"));
    for turn in 0..4 {
        conv.add_user_message(format!("q{turn}"));
        conv.add_assistant_message(Some(format!("a{turn}")), None);
    }

    // A window that would start on the checkpoint message.
    conv.compact_history(8);

    assert!(
        conv.messages
            .iter()
            .any(|m| m.content_str() == Some("CHECKPOINT SUMMARY OF EARLIER WORK")),
        "the model-written summary must survive: {:?}",
        conv.messages
    );
}
