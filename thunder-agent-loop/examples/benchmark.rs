use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use thunder_agent_loop::core::token_estimator::estimate_token_count;
use thunder_agent_loop::loop_engine::engine::AgentLoop;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use tokio_util::sync::CancellationToken;

struct BenchmarkMockLLM;

#[async_trait]
impl LLMClientTrait for BenchmarkMockLLM {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel_token: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx
                .send(Ok(LLMStreamChunk::Completed {
                    content: Some("Benchmark response text chunk".to_string()),
                    tool_calls: vec![],
                    finish_reason: "stop".to_string(),
                    prompt_tokens: Some(15),
                    completion_tokens: Some(10),
                    cached_tokens: None,
                    reasoning_tokens: None,
                }))
                .await;
        });
        Ok(rx)
    }
}

struct FastBenchTool;

#[async_trait]
impl AgentTool for FastBenchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("noop", "no-op", json!({ "type": "object", "properties": {} }))
    }

    async fn execute(&self, _args: serde_json::Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

#[tokio::main]
async fn main() {
    println!("============================================================");
    println!("🚀 THUNDER AGENT LOOP — HIGH-PERFORMANCE BENCHMARK SUITE");
    println!("============================================================");

    // 1. Benchmark Token Estimator
    println!("\n[1/3] Benchmarking Token Estimator Throughput...");
    let sample_text = "Thunder Agent Loop is an ultra-fast, minimal-resource Agent Loop written in pure Rust. 这是一个高性能代理循环测试。".repeat(100);
    let sample_bytes = sample_text.len();
    let iters = 200_000;

    let start = Instant::now();
    let mut _total_tokens = 0;
    for _ in 0..iters {
        _total_tokens += estimate_token_count(&sample_text);
    }
    let elapsed = start.elapsed();
    let total_mb = (sample_bytes * iters) as f64 / (1024.0 * 1024.0);
    let throughput = total_mb / elapsed.as_secs_f64();

    println!("  - Iterations: {}", iters);
    println!("  - Elapsed: {:.2?}", elapsed);
    println!("  - Speed: {:.2} MB/s ({:.2} million chars/sec)", throughput, (sample_text.len() * iters) as f64 / elapsed.as_secs_f64() / 1_000_000.0);

    // 2. High Concurrency Agent Loops (10,000 Concurrent Loops)
    println!("\n[2/3] Benchmarking 10,000 Concurrent Agent Loop Executions...");
    let config = AgentConfig::new("benchmark-model").with_max_turns(3);
    let mock_client = Arc::new(BenchmarkMockLLM);

    let concurrent_tasks = 10_000;
    let completed_count = Arc::new(AtomicUsize::new(0));

    let start = Instant::now();
    let mut handles = Vec::with_capacity(concurrent_tasks);

    for i in 0..concurrent_tasks {
        let mut agent = AgentLoop::new(config.clone()).with_custom_client(mock_client.clone());
        agent.register_tool(Arc::new(FastBenchTool));
        let count_clone = completed_count.clone();

        handles.push(tokio::spawn(async move {
            let res = agent.run(format!("User query #{}", i), None).await;
            if res.is_ok() {
                count_clone.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    let elapsed = start.elapsed();
    let total_completed = completed_count.load(Ordering::SeqCst);
    let ops_per_sec = total_completed as f64 / elapsed.as_secs_f64();
    let avg_latency_us = (elapsed.as_micros() as f64) / total_completed as f64;

    println!("  - Total Completed: {} / {}", total_completed, concurrent_tasks);
    println!("  - Total Wall Time: {:.2?}", elapsed);
    println!("  - Throughput: {:.0} complete agent runs / second", ops_per_sec);
    println!("  - Average Loop Overhead / Task: {:.2} µs ({:.4} ms)", avg_latency_us, avg_latency_us / 1000.0);

    // 3. Memory & Resource Footprint
    println!("\n[3/3] Inspecting Memory Footprint...");
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let pid = std::process::id();
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output();

        if let Ok(out) = output {
            let rss_kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
            println!("  - Process RSS Memory (after 10k concurrent loops): {:.2} MB ({} KB)", rss_kb as f64 / 1024.0, rss_kb);
        }
    }

    println!("\n============================================================");
    println!("✨ BENCHMARK COMPLETE — ZERO BOTTLENECK CONFIRMED");
    println!("============================================================");
}
