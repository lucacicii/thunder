use async_trait::async_trait;
use std::env;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::prelude::*;
use thunder_agent_root::prelude::*;
use tokio_util::sync::CancellationToken;

struct CliMockClient {
    turn: AtomicUsize,
}

#[async_trait]
impl LLMClientTrait for CliMockClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let turn = self.turn.fetch_add(1, Ordering::SeqCst);

        tokio::spawn(async move {
            if turn == 0 {
                let text = "I am ThunderRoot. Let me inspect the project structure with bash tool.\n";
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
                            "{\"command\":\"echo \\\"ThunderRoot Kernel is operational with dynamic plugins!\\\"\"}",
                        )],
                        finish_reason: "tool_calls".to_string(),
                        prompt_tokens: Some(options.messages.len() * 15),
                        completion_tokens: Some(30),
                    }))
                    .await;
            } else {
                let text = "ThunderRoot execution finished successfully. All active plugins responded as expected.";
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
                    }))
                    .await;
            }
        });

        Ok(rx)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let prompt = if args.len() > 1 && !args[1].starts_with("--") {
        args[1].clone()
    } else {
        "Review repository architecture and manage conversation session".to_string()
    };

    let use_mock = args.iter().any(|a| a == "--mock");

    let model = env::var("MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
    let base_cfg = AgentConfig::new(model).with_unlimited_turns();

    // 1. Initialize ThunderRoot with extensible plugins
    let mut root = ThunderRoot::new(base_cfg.clone())
        .with_plugin(ConversationPlugin::with_memory_store())
        .with_plugin(SkillsPlugin::default())
        .with_plugin(McpPlugin::default());

    #[cfg(feature = "orchestra")]
    {
        let orch_cfg = thunder_orchestra::OrchestraConfig::new(thunder_orchestra::Topology::Auto)
            .with_base(base_cfg);
        root = root.with_plugin(OrchestraPlugin::new(orch_cfg));
    }

    println!("============================================================");
    println!("⚡ Thunder-Root Microkernel Host CLI");
    println!("============================================================");
    println!("▶ User Prompt: {}", prompt);
    println!("▶ Mode: {}", if use_mock { "MOCK (deterministic)" } else { "LIVE LLM" });
    println!("▶ Registered Plugins in Registry: {:?}", 
        root.registry().list_manifests().iter().map(|m| &m.id).collect::<Vec<_>>()
    );
    println!("------------------------------------------------------------");

    let custom_client: Option<Arc<dyn LLMClientTrait>> = if use_mock {
        Some(Arc::new(CliMockClient {
            turn: AtomicUsize::new(0),
        }))
    } else {
        None
    };

    let options = RootRunOptions {
        session_id: Some("cli_session_1".to_string()),
        use_mock,
        custom_client,
        cancellation_token: None,
        forced_plugins: None,
        register_builtins: true,
        thinking_level: None,
    };

    let mut handle = root.execute(prompt, options).await?;
    println!("🔍 Dynamic Plugin Selection Result:");
    println!("   Active Plugins : {:?}", handle.selection.active_plugin_ids);
    println!("   Decision Reason: {}", handle.selection.reason);
    println!("   Confidence     : {:.2}", handle.selection.confidence);
    println!("------------------------------------------------------------");

    if let Some(mut rx) = handle.take_events() {
        tokio::spawn(async move {
            while let Some(observed) = rx.recv().await {
                match observed.event {
                    AgentEvent::TokenDelta { delta, .. } => {
                        print!("{delta}");
                    }
                    AgentEvent::ToolExecStart { name, .. } => {
                        println!("\n⚙️  Tool Executing: {name}");
                    }
                    AgentEvent::ToolExecResult { name, result, .. } => {
                        println!("✔  Tool Completed: {name} ({}ms)", result.duration_ms);
                    }
                    _ => {}
                }
            }
        });
    }

    let result = handle.join().await?;
    println!("\n------------------------------------------------------------");
    println!("🏁 Root Execution Finished (Status: {:?})", result.run_result.finish_reason);
    println!("📊 Total Turns: {}, Tool Executions: {}", 
        result.run_result.stats.total_turns, 
        result.run_result.stats.total_tool_executions
    );
    if let Some(final_text) = result.final_content {
        println!("📝 Final Output:\n{}", final_text);
    }
    println!("============================================================");

    Ok(())
}
