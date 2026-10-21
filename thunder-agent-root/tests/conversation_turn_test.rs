//! The conversation plugin's transcript shape: one entry per engine turn.
//!
//! A turn does not arrive ready-made — its text and its chain-of-thought stream
//! in as hundreds of deltas, and its tool calls and their results interleave —
//! so the plugin buffers a turn and writes it whole at `turn_end`. What these
//! cases pin down is the shape a reader gets back: the turn's own message,
//! carrying the reasoning and the stats nothing else records, followed by the
//! results that answered it.

#![cfg(feature = "conversation")]

use thunder_agent_loop::types::event::{AgentEvent, FinishReason, ObservedEvent, TurnStats};
use thunder_agent_loop::types::message::{ChatMessage, ToolCall};
use thunder_agent_types::tool::ToolExecutionResult;
use thunder_agent_root::plugins::conversation::ConversationPlugin;
use thunder_agent_root::prelude::*;

fn observed(event: AgentEvent) -> ObservedEvent {
    ObservedEvent {
        agent_id: "agent".to_string(),
        event,
        timestamp_ms: 1,
    }
}

fn turn_stats(turn: usize, duration_ms: u64, tool_calls_count: usize) -> TurnStats {
    TurnStats {
        turn,
        prompt_tokens: Some(100 * turn),
        completion_tokens: Some(20 * turn),
        duration_ms,
        tool_calls_count,
        ..TurnStats::default()
    }
}

fn tool_result(output: &str) -> ToolExecutionResult {
    ToolExecutionResult {
        output: output.to_string(),
        is_error: false,
        truncated: false,
        original_bytes: output.len(),
        duration_ms: 0,
        telemetry: None,
    }
}

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall::new_function(id, name, "{}")
}

/// Initialize a plugin for `sess` and return it with its context.
async fn plugin_with(prompt: &str) -> (ConversationPlugin, PluginContext) {
    let plugin = ConversationPlugin::with_memory_store();
    let ctx = PluginContext::new("sess").with_prompt(prompt);
    plugin.on_init(&ctx).await.unwrap();
    (plugin, ctx)
}

/// The messages the plugin has written so far, cloned out of its live state.
async fn messages(plugin: &ConversationPlugin) -> Vec<ChatMessage> {
    let active = plugin.active_conversation();
    let guard = active.read().await;
    guard
        .as_ref()
        .expect("a conversation is active")
        .messages
        .clone()
}

/// The first assistant message written — the first turn's own entry.
fn first_assistant(messages: &[ChatMessage]) -> &ChatMessage {
    messages
        .iter()
        .find(|message| matches!(message, ChatMessage::Assistant { .. }))
        .expect("a turn should be an assistant message")
}

/// The system message is plumbing (every request re-splices it) and every host
/// filters it out, so it is filtered here too.
fn transcript(messages: &[ChatMessage]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| !matches!(message, ChatMessage::System { .. }))
        .map(|message| match message {
            ChatMessage::System { .. } => unreachable!("filtered above"),
            ChatMessage::User { content, .. } => format!("user: {content}"),
            ChatMessage::Assistant { content, .. } => {
                format!("assistant: {}", content.as_deref().unwrap_or("<none>"))
            }
            ChatMessage::Tool { content, .. } => format!("tool: {content}"),
        })
        .collect()
}

#[tokio::test]
async fn a_turn_is_written_whole_at_its_end() {
    let (plugin, ctx) = plugin_with("do the thing").await;

    plugin
        .on_event(
            &observed(AgentEvent::TurnStart {
                turn: 1,
                timestamp: 1,
                estimated_prompt_tokens: 10,
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ReasoningDelta {
                turn: 1,
                delta: "先看目录".to_string(),
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ReasoningDelta {
                turn: 1,
                delta: "，再读文件".to_string(),
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ToolCallReady {
                turn: 1,
                tool_call: call("c1", "read_file"),
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ToolExecResult {
                turn: 1,
                tool_call_id: "c1".to_string(),
                name: "read_file".to_string(),
                result: tool_result("file body"),
            }),
            &ctx,
        )
        .await;
    // Not written yet: the turn is not whole until its `turn_end`.
    assert_eq!(
        transcript(&messages(&plugin).await),
        vec!["user: do the thing"]
    );

    plugin
        .on_event(
            &observed(AgentEvent::TokenDelta {
                turn: 1,
                delta: "读完了".to_string(),
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::TurnEnd {
                turn: 1,
                finish_reason: "done".to_string(),
                stats: turn_stats(1, 1500, 1),
            }),
            &ctx,
        )
        .await;

    // One message for the turn — its text, its reasoning, its call — then the
    // result that answered the call.
    assert_eq!(
        transcript(&messages(&plugin).await),
        vec!["user: do the thing", "assistant: 读完了", "tool: file body",]
    );

    let written = messages(&plugin).await;
    let ChatMessage::Assistant {
        tool_calls,
        reasoning,
        stats,
        ..
    } = first_assistant(&written)
    else {
        unreachable!("found by variant")
    };
    assert_eq!(reasoning.as_deref(), Some("先看目录，再读文件"));
    assert_eq!(
        tool_calls
            .as_ref()
            .map(|calls| calls.iter().map(|c| c.id.clone()).collect::<Vec<_>>()),
        Some(vec!["c1".to_string()])
    );
    assert_eq!(stats.as_ref().map(|s| s.turn), Some(1));
    assert_eq!(stats.as_ref().map(|s| s.duration_ms), Some(1500));
    assert_eq!(stats.as_ref().and_then(|s| s.completion_tokens), Some(20));

    // A host reads this back out of `conversation.json`, so the keys are the
    // contract: `stats.turn` numbers a turn's dot, `reasoning` is what that dot
    // opens, and `tool_calls` is what the turn's tools are recovered from.
    let json = serde_json::to_value(first_assistant(&written)).unwrap();
    assert_eq!(json["role"], "assistant");
    assert_eq!(json["content"], "读完了");
    assert_eq!(json["reasoning"], "先看目录，再读文件");
    assert_eq!(json["stats"]["turn"], 1);
    assert_eq!(json["tool_calls"][0]["id"], "c1");
    assert_eq!(json["tool_calls"][0]["function"]["name"], "read_file");
}

#[tokio::test]
async fn each_turn_is_its_own_entry_with_its_own_results() {
    let (plugin, ctx) = plugin_with("two rounds").await;

    for turn in 1..=2usize {
        plugin
            .on_event(
                &observed(AgentEvent::TurnStart {
                    turn,
                    timestamp: turn as u64,
                    estimated_prompt_tokens: 10,
                }),
                &ctx,
            )
            .await;
        plugin
            .on_event(
                &observed(AgentEvent::ReasoningDelta {
                    turn,
                    delta: format!("想{turn}"),
                }),
                &ctx,
            )
            .await;
        plugin
            .on_event(
                &observed(AgentEvent::ToolCallReady {
                    turn,
                    tool_call: call(&format!("c{turn}"), "read_file"),
                }),
                &ctx,
            )
            .await;
        plugin
            .on_event(
                &observed(AgentEvent::ToolExecResult {
                    turn,
                    tool_call_id: format!("c{turn}"),
                    name: "read_file".to_string(),
                    result: tool_result(&format!("out{turn}")),
                }),
                &ctx,
            )
            .await;
        plugin
            .on_event(
                &observed(AgentEvent::TokenDelta {
                    turn,
                    delta: format!("答{turn}"),
                }),
                &ctx,
            )
            .await;
        plugin
            .on_event(
                &observed(AgentEvent::TurnEnd {
                    turn,
                    finish_reason: "tool_calls".to_string(),
                    stats: turn_stats(turn, 10 * turn as u64, 1),
                }),
                &ctx,
            )
            .await;
    }

    // A provider reads this back as one assistant message per turn, each
    // followed by the results of its own calls.
    assert_eq!(
        transcript(&messages(&plugin).await),
        vec![
            "user: two rounds",
            "assistant: 答1",
            "tool: out1",
            "assistant: 答2",
            "tool: out2",
        ]
    );

    let turns: Vec<usize> = messages(&plugin)
        .await
        .iter()
        .filter_map(|message| message.turn_stats().map(|stats| stats.turn))
        .collect();
    assert_eq!(turns, vec![1, 2]);
}

#[tokio::test]
async fn a_turn_without_a_turn_end_is_still_written() {
    let (plugin, ctx) = plugin_with("stop halfway").await;

    plugin
        .on_event(
            &observed(AgentEvent::TurnStart {
                turn: 1,
                timestamp: 1,
                estimated_prompt_tokens: 10,
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ReasoningDelta {
                turn: 1,
                delta: "想了半句".to_string(),
            }),
            &ctx,
        )
        .await;
    plugin
        .on_event(
            &observed(AgentEvent::ToolCallReady {
                turn: 1,
                tool_call: call("c1", "read_file"),
            }),
            &ctx,
        )
        .await;
    // The answer streamed before the stop; that partial text is the run's final
    // content, which is why `on_finish` must not write it a second time.
    plugin
        .on_event(
            &observed(AgentEvent::TokenDelta {
                turn: 1,
                delta: "半".to_string(),
            }),
            &ctx,
        )
        .await;

    // The run is cancelled: no `turn_end`, and a partial answer as the result's
    // final content.
    let result = thunder_agent_loop::AgentRunResult {
        agent_id: "agent".to_string(),
        final_content: Some("半".to_string()),
        messages: Vec::new(),
        raw_messages: None,
        stats: Default::default(),
        finish_reason: FinishReason::Cancelled,
    };
    plugin.on_finish(&result, &ctx).await.unwrap();

    // The buffered turn survives without its stats, and the partial answer is
    // not written a second time on top of it.
    assert_eq!(
        transcript(&messages(&plugin).await),
        vec!["user: stop halfway", "assistant: 半"]
    );
    let written = messages(&plugin).await;
    let ChatMessage::Assistant {
        reasoning,
        tool_calls,
        stats,
        ..
    } = first_assistant(&written)
    else {
        unreachable!("found by variant")
    };
    assert_eq!(reasoning.as_deref(), Some("想了半句"));
    assert_eq!(tool_calls.as_ref().map(Vec::len), Some(1));
    assert!(stats.is_none(), "a turn without `turn_end` has no stats");
}
