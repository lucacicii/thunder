use async_trait::async_trait;
use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use tokio_util::sync::CancellationToken;

struct MockLLMClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for MockLLMClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn == 0 {
                // First turn: model decides to run bash command
                let text = "I will check the current system information and date using bash tool.\n";
                for ch in text.chars() {
                    let _ = tx.send(Ok(LLMStreamChunk::Token(ch.to_string()))).await;
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }

                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![ToolCall::new_function(
                            "call_bash_1",
                            "bash",
                            "{\"command\":\"echo \\\"Hello from Thunder Agent Loop running in $(uname -s) ($(uname -m)) at $(date)\\\"\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 15),
                        completion_tokens: Some(30),
                        cached_tokens: None,
                    }))
                    .await;
            } else {
                // Second turn: summarize results
                let text = "Here is the summary of the command output:\nThe system environment has been verified successfully!";
                for word in text.split(' ') {
                    let _ = tx.send(Ok(LLMStreamChunk::Token(format!("{} ", word)))).await;
                    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
                }

                let _ = tx
                    .send(Ok(LLMStreamChunk::Completed {
                        content: Some(text.to_string()),
                        tool_calls: vec![],
                        finish_reason: "stop".to_string(),
                        prompt_tokens: Some(options.messages.len() * 15),
                        completion_tokens: Some(25),
                        cached_tokens: None,
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize structured logging (controlled via RUST_LOG environment variable)
    init_logger();

    let args: Vec<String> = env::args().collect();
    let prompt = if args.len() > 1 && !args[1].starts_with("--") {
        args[1].clone()
    } else {
        "Please check the system date and status".to_string()
    };

    let is_mock = args.iter().any(|a| a == "--mock");

    let model = env::var("MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
    let config = AgentConfig::new(model)
        .with_system_prompt("You are Thunder Agent, an ultra-fast autonomous coding assistant.")
        .with_unlimited_turns();

    let mut agent = if is_mock {
        println!("🤖 [Mode: Local Mock LLM Engine (Unlimited Turns)]");
        AgentLoop::new(config).with_custom_client(Arc::new(MockLLMClient {
            turn: AtomicUsize::new(0),
        }))
    } else {
        println!("🌐 [Mode: Live Online LLM API (Unlimited Turns)]");
        AgentLoop::new(config)
    };

    // Register built-in tools
    agent.register_tool(Arc::new(BashTool::default()));
    agent.register_tool(Arc::new(ReadFileTool::default()));
    agent.register_tool(Arc::new(WriteFileTool::default()));

    println!("⚡ User Prompt: \"{}\"", prompt);
    println!("────────────────────────────────────────────────────────────");

    let mut event_rx = agent.subscribe_events();
    tokio::spawn(async move {
        while let Ok(observed) = event_rx.recv().await {
            match observed.event {
                AgentEvent::TurnStart { turn, .. } => {
                    println!("\n🔄 [{}] [Turn {} Started]", observed.agent_id, turn);
                }
                AgentEvent::ReasoningDelta { delta, .. } => {
                    print!("\x1b[2m{}\x1b[0m", delta); // Render reasoning in dimmed style
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                AgentEvent::TokenDelta { delta, .. } => {
                    print!("{}", delta);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
                AgentEvent::ToolExecResult { name, result, .. } => {
                    println!("\n🛠️  [{}] [Tool '{}' executed in {} ms]", observed.agent_id, name, result.duration_ms);
                    println!("    Output: {}", result.output.lines().next().unwrap_or(""));
                }
                AgentEvent::TurnEnd { finish_reason, stats, .. } => {
                    println!("\n🏁 [{}] [Turn Finished: reason='{}', duration={}ms]", observed.agent_id, finish_reason, stats.duration_ms);
                }
                AgentEvent::Error { turn, message, .. } => {
                    eprintln!("\n❌ [{}] [Error in Turn {:?}: {}]", observed.agent_id, turn, message);
                }
                _ => {}
            }
        }
    });

    let res = agent.run(prompt, None).await?;

    println!("────────────────────────────────────────────────────────────");
    println!("✨ [Loop Completed]");
    println!("   Finish Reason: {:?}", res.finish_reason);
    println!("   Total Turns:   {}", res.stats.total_turns);
    println!("   Tool Calls:    {}", res.stats.total_tool_executions);
    println!("   Total Time:    {} ms", res.stats.total_duration_ms);

    if let Some(final_text) = &res.final_content {
        println!("\n📋 [Final Answer]:\n{}", final_text);
    } else {
        println!("\n(No final text output)");
    }

    Ok(())
}
