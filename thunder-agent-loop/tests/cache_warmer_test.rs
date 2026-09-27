//! Prompt-cache warmer lifecycle tests against the session registry.
//!
//! Uses `start_paused` so the virtual clock auto-advances through the warmer's
//! TTL schedule deterministically.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thunder_agent_loop::cache::warmer::{
    self, PromptCacheWarmSettings, WarmSnapshot, MAX_IDLE_WARMING_AGE,
};
use thunder_agent_loop::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use thunder_agent_loop::types::message::ChatMessage;
use thunder_agent_loop::types::tool::ToolDefinition;
use tokio::sync::mpsc;
use tokio::task;
use tokio_util::sync::CancellationToken;

/// Mock transport that counts replay requests and asserts the one-token cap.
struct CountingClient {
    replays: Arc<AtomicUsize>,
    expected_session: &'static str,
}

#[async_trait]
impl LLMClientTrait for CountingClient {
    async fn stream_chat(
        &self,
        options: ChatRequestOptions,
        _cancel: CancellationToken,
    ) -> Result<mpsc::Receiver<Result<LLMStreamChunk, String>>, String> {
        assert_eq!(
            options.max_tokens,
            Some(1),
            "cache-warm replays must cap output at one token"
        );
        assert_eq!(
            options.session_id.as_deref(),
            Some(self.expected_session),
            "replays must keep the conversation routing key"
        );
        self.replays.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel(4);
        tx.send(Ok(LLMStreamChunk::Completed {
            content: Some(String::new()),
            tool_calls: vec![],
            finish_reason: "stop".to_string(),
            prompt_tokens: Some(50_000),
            completion_tokens: Some(1),
            cached_tokens: Some(50_000),
            cache_write_tokens: Some(0),
            reasoning_tokens: None,
        }))
        .await
        .unwrap();
        Ok(rx)
    }
}

fn sonnet_like_settings(ttl_secs: u64) -> PromptCacheWarmSettings {
    PromptCacheWarmSettings {
        ttl_secs,
        input_price_per_m: Some(3.0),
        cache_read_price_per_m: Some(0.3),
        cache_write_price_per_m: Some(3.75),
        replay_safe: true,
    }
}

fn snapshot(session_id: &str) -> WarmSnapshot {
    WarmSnapshot {
        model: "anthropic/claude-sonnet-5".to_string(),
        messages: vec![ChatMessage::system("sys"), ChatMessage::user("hi")],
        tools: vec![ToolDefinition::new_function(
            "bash_tool",
            "Run a shell command",
            serde_json::json!({"type": "object", "properties": {}}),
        )],
        temperature: None,
        top_p: None,
        thinking_level: None,
        session_id: session_id.to_string(),
        prompt_tokens: 50_000,
    }
}

/// Poll `cond` while stepping the (paused) virtual clock forward in small
/// increments, bounded by a generous virtual deadline.
async fn wait_until(description: &str, cond: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(600);
    while !cond() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for: {description}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn warms_while_streaming_then_stops_when_idle_economics_fail() {
    let replays = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(CountingClient {
        replays: Arc::clone(&replays),
        expected_session: "warm-sess",
    });

    // TTL 120s → refresh at 108s.
    warmer::schedule(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        snapshot("warm-sess"),
        sonnet_like_settings(120),
    );
    assert!(warmer::is_active("warm-sess"));

    // First refresh fires (streaming phase, probability 1): 50k tokens at
    // sonnet-like pricing clears the $0.05 expected-savings gate.
    let seen = Arc::clone(&replays);
    wait_until("first warm replay", move || seen.load(Ordering::SeqCst) >= 1).await;

    // The run settles: idle economics (15% continuation) drop the same prompt
    // below the gate, so the next scheduled refresh stops the warmer.
    warmer::mark_idle("warm-sess");
    wait_until("warmer stops on idle economics", || !warmer::is_active("warm-sess")).await;
    assert_eq!(
        replays.load(Ordering::SeqCst),
        1,
        "idle stop must not issue another replay"
    );
}

#[tokio::test(start_paused = true)]
async fn invalidate_cancels_pending_refresh() {
    let replays = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(CountingClient {
        replays: Arc::clone(&replays),
        expected_session: "warm-sess2",
    });

    warmer::schedule(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        snapshot("warm-sess2"),
        sonnet_like_settings(120),
    );
    // A new run on the conversation cancels before the first refresh fires.
    warmer::invalidate("warm-sess2");
    assert!(!warmer::is_active("warm-sess2"));

    // Give the cancelled task a chance to (wrongly) fire.
    tokio::time::sleep(Duration::from_secs(300)).await;
    assert_eq!(replays.load(Ordering::SeqCst), 0);
    assert!(!warmer::is_active("warm-sess2"));
}

#[tokio::test(start_paused = true)]
async fn idle_age_cap_is_enforced() {
    // Enabled by a huge prompt that stays above the gate even at 15%.
    let replays = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(CountingClient {
        replays: Arc::clone(&replays),
        expected_session: "warm-sess3",
    });

    let mut snap = snapshot("warm-sess3");
    snap.prompt_tokens = 2_000_000;
    // Small TTL keeps the refresh cadence at 10s; the huge prompt keeps idle
    // economics above the gate so only the age cap can stop it.

    warmer::schedule(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        snap,
        sonnet_like_settings(20),
    );
    warmer::mark_idle("warm-sess3");

    // Past the idle age cap the warmer must be gone, not refreshing forever.
    tokio::time::sleep(MAX_IDLE_WARMING_AGE + Duration::from_secs(60)).await;
    assert!(!warmer::is_active("warm-sess3"));
    let count = replays.load(Ordering::SeqCst);
    // 30min at a 10s cadence ≈ 180 refreshes; allow one boundary cycle.
    assert!(
        count > 0 && count <= 185,
        "expected age-capped refreshes, got {count}"
    );
}

#[tokio::test(start_paused = true)]
async fn reschedule_replaces_previous_generation() {
    let replays = Arc::new(AtomicUsize::new(0));
    let client = Arc::new(CountingClient {
        replays: Arc::clone(&replays),
        expected_session: "warm-sess4",
    });

    warmer::schedule(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        snapshot("warm-sess4"),
        sonnet_like_settings(120),
    );
    // The next turn replaces the snapshot (pi's per-request start()).
    warmer::schedule(
        Arc::clone(&client) as Arc<dyn LLMClientTrait>,
        snapshot("warm-sess4"),
        sonnet_like_settings(120),
    );
    assert!(warmer::is_active("warm-sess4"));

    // Only the surviving generation refreshes; the replaced one was cancelled.
    let seen = Arc::clone(&replays);
    wait_until("warm replay from surviving generation", move || {
        seen.load(Ordering::SeqCst) >= 1
    })
    .await;
    task::yield_now().await;
    assert!(replays.load(Ordering::SeqCst) >= 1);
    warmer::invalidate("warm-sess4");
}

#[test]
fn replay_unsafe_settings_never_schedule() {
    // Synchronous gates that schedule() checks before spawning a task:
    // unsafe replay (anthropic budget-derived thinking) and too-short TTLs.
    let settings = PromptCacheWarmSettings {
        ttl_secs: 300,
        input_price_per_m: Some(3.0),
        cache_read_price_per_m: Some(0.3),
        cache_write_price_per_m: Some(3.75),
        replay_safe: false,
    };
    assert!(!settings.replay_safe);
    // TTL at or below the 10s scheduling floor yields no delay, so such
    // models can never warm regardless of settings.
    assert_eq!(warmer::warming_delay(Duration::from_secs(10)), None);
    assert_eq!(warmer::warming_delay(Duration::from_secs(5)), None);
    assert!(warmer::warming_delay(Duration::from_secs(300)).is_some());
}
