//! KERNEL.md §8 anchor 9: compaction concurrency is bounded.
//!
//! Every agent compacts at roughly the same moment — same model, same window,
//! same threshold — so the pressure on the provider is the sum across agents.
//! The cap is a host-owned limiter shared by every agent it runs; these tests
//! pin that the limiter actually holds.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::pruning::checkpoint::Summarizer;
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::config::AgentConfig;
use tokio_util::sync::CancellationToken;

/// Records how many summarization requests were in flight at once.
#[derive(Default)]
struct SlowClient {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
}

impl SlowClient {
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMClientTrait for SlowClient {
    async fn stream_chat(
        &self,
        _options: ChatRequestOptions,
        _cancel: CancellationToken,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);

        // Long enough for the other tasks to pile up behind the limiter.
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let _ = tx
            .send(Ok(LLMStreamChunk::Completed {
                content: Some("summary".to_string()),
                tool_calls: vec![],
                finish_reason: "stop".to_string(),
                prompt_tokens: Some(1),
                completion_tokens: Some(1),
                cached_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }))
            .await;
        Ok(rx)
    }
}

fn config(max_concurrent_summaries: usize) -> AgentConfig {
    let mut config = AgentConfig::new("test-model");
    config.pruning.max_concurrent_summaries = max_concurrent_summaries;
    config
}

/// Six agents compacting at once through one limiter of two: at most two
/// summarization requests are ever in flight.
#[tokio::test]
async fn concurrent_compactions_stay_under_the_limit() {
    let client = Arc::new(SlowClient::default());
    let config = config(2);
    let limit = Arc::new(tokio::sync::Semaphore::new(2));

    let mut tasks = Vec::new();
    for i in 0..6 {
        let summarizer = Summarizer::new(
            Arc::clone(&client) as Arc<dyn LLMClientTrait>,
            &config,
            Arc::clone(&limit),
        );
        tasks.push(tokio::spawn(async move {
            summarizer
                .summarize(
                    &format!("history {i}"),
                    None,
                    &[],
                    &[],
                    &CancellationToken::new(),
                )
                .await
        }));
    }

    for task in tasks {
        task.await
            .expect("no panic")
            .expect("every summarization succeeds");
    }

    assert_eq!(client.calls(), 6, "every agent did summarize");
    assert!(
        client.peak() <= 2,
        "peak in flight {} exceeded the limit of 2",
        client.peak()
    );
    assert_eq!(
        client.peak(),
        2,
        "the limiter must not serialize everything either"
    );
}

/// The limiter is shared, not copied: two agents built with different configs
/// but the same limiter still share the budget.
#[tokio::test]
async fn a_shared_limiter_is_the_budget_regardless_of_config() {
    let client = Arc::new(SlowClient::default());
    let limit = Arc::new(tokio::sync::Semaphore::new(1));

    let a = Summarizer::new(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        &config(8),
        Arc::clone(&limit),
    );
    let b = Summarizer::new(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        &config(8),
        Arc::clone(&limit),
    );

    let cancel = CancellationToken::new();
    let (ra, rb) = tokio::join!(
        a.summarize("one", None, &[], &[], &cancel),
        b.summarize("two", None, &[], &[], &cancel),
    );
    ra.expect("first");
    rb.expect("second");

    assert_eq!(
        client.peak(),
        1,
        "one permit means the second request waits, whatever the configs say"
    );
}
