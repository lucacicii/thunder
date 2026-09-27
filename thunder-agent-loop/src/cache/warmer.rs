//! Prompt-cache warming: keep one conversation's provider prompt-cache entry
//! alive by replaying its last request with a one-token output cap before the
//! cache TTL expires.
//!
//! Port of pi-coding-agent's `CacheWarmer` (core/cache-warmer.ts), adapted to
//! thunder's ownership model:
//!
//! - The engine re-schedules a warmer **after every successful turn** (each
//!   schedule replaces the previous task, exactly like pi's
//!   `cacheWarmer.start()` per request).
//! - When a run settles, the engine calls [`mark_idle`]: economics switch from
//!   "the next request is certain" (probability 1) to a 15% idle continuation
//!   estimate, and the task is capped at [`MAX_IDLE_WARMING_AGE`] from its
//!   last real request.
//! - A new run on the same conversation calls [`invalidate`], cancelling any
//!   warmer still holding the stale pre-run prefix.
//!
//! Warming requires the model to declare **both** a prompt-cache lifetime and
//! pricing in `models.json` (`promptCache` + `cost`): without prices the
//! expected-savings gate cannot be evaluated and warming stays off, matching
//! pi's "cache economics unavailable" rule.
//!
//! Refreshes never enter model context; usage is logged (best-effort).

use crate::stream::client::{ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use crate::types::message::ChatMessage;
use crate::types::tool::ToolDefinition;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Chance that a real request arrives before the cache entry expires while the
/// owning run sits idle (pi's measured constant; per-session estimates were
/// not better).
pub const IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;

/// A refresh is sent only when it is expected to save at least this much.
pub const MIN_EXPECTED_SAVINGS_USD: f64 = 0.05;

/// Idle warming stops this long after the last real request; continuation
/// estimates become less reliable with age.
pub const MAX_IDLE_WARMING_AGE: Duration = Duration::from_secs(30 * 60);

/// Prompt-cache-relevant pricing (per million tokens) and lifetime for one
/// model. Derived from `models.json` (`promptCache` + `cost`) by
/// `ModelSpec::prompt_cache_warm_settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCacheWarmSettings {
    /// Best-effort cache lifetime in seconds for the active retention tier.
    pub ttl_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price_per_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_price_per_m: Option<f64>,
    /// `false` when replaying with `max_tokens = 1` would change the cached
    /// prefix key (Anthropic budget-derived thinking derives `budget_tokens`
    /// from `max_tokens`, and the budget is part of the cache key).
    #[serde(
        default = "replay_safe_default",
        skip_serializing_if = "is_replay_safe"
    )]
    pub replay_safe: bool,
}

fn replay_safe_default() -> bool {
    true
}

fn is_replay_safe(v: &bool) -> bool {
    *v
}

/// Refresh at 90% of the TTL while preserving at least ten seconds of margin.
/// `None` when the TTL is too short to schedule reliably.
pub fn warming_delay(ttl: Duration) -> Option<Duration> {
    let ttl_ms = ttl.as_millis() as u64;
    if ttl_ms <= 10_000 {
        return None;
    }
    Some(Duration::from_millis(
        (ttl_ms * 9 / 10).min(ttl_ms - 10_000).max(1),
    ))
}

#[derive(Debug, Clone, PartialEq)]
pub enum CacheWarmDecision {
    Warm,
    Stop(&'static str),
}

/// Economics gate, mirroring pi's `CacheWarmer.evaluate`:
/// `expected = continuation_probability × miss_cost − warm_cost`, warmed only
/// when `expected ≥ $0.05`. Returns the decision and the expected savings.
pub fn decide_warming(
    prompt_tokens: usize,
    idle: bool,
    settings: &PromptCacheWarmSettings,
) -> (CacheWarmDecision, f64) {
    let Some((input, read, write)) = full_prices(settings) else {
        return (CacheWarmDecision::Stop("cache economics unavailable"), 0.0);
    };
    if prompt_tokens == 0 {
        return (CacheWarmDecision::Stop("no prompt tokens"), 0.0);
    }
    // A refresh re-reads the whole prompt at the cache-read rate (the single
    // output token is negligible). A miss re-bills it at the write (or plain
    // input) rate instead of the read rate.
    let warm_cost = prompt_tokens as f64 * read / 1_000_000.0;
    let miss_rate = if write > 0.0 { write } else { input };
    let miss_cost = prompt_tokens as f64 * (miss_rate - read).max(0.0) / 1_000_000.0;
    let probability = if idle {
        IDLE_CONTINUATION_PROBABILITY
    } else {
        1.0
    };
    let expected = probability * miss_cost - warm_cost;
    let decision = if expected >= MIN_EXPECTED_SAVINGS_USD {
        CacheWarmDecision::Warm
    } else {
        CacheWarmDecision::Stop("expected savings below threshold")
    };
    (decision, expected)
}

fn full_prices(settings: &PromptCacheWarmSettings) -> Option<(f64, f64, f64)> {
    match (
        settings.input_price_per_m,
        settings.cache_read_price_per_m,
        settings.cache_write_price_per_m,
    ) {
        (Some(input), Some(read), Some(write)) if read > 0.0 && (write > 0.0 || input > 0.0) => {
            Some((input, read, write))
        }
        _ => None,
    }
}

/// Immutable replay snapshot of the last real request on a branch.
#[derive(Debug, Clone)]
pub struct WarmSnapshot {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub thinking_level: Option<String>,
    pub session_id: String,
    /// Prompt size of the last real request as reported by the provider.
    pub prompt_tokens: usize,
}

struct ActiveWarm {
    token: CancellationToken,
    idle: Arc<AtomicBool>,
    generation: u64,
}

fn registry() -> &'static Mutex<HashMap<String, ActiveWarm>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, ActiveWarm>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Cancel any warmer still holding the pre-run prefix of this conversation.
/// The engine calls this when a new run starts on the session.
pub fn invalidate(session_id: &str) {
    if let Some(warm) = registry().lock().unwrap().remove(session_id) {
        warm.token.cancel();
    }
}

/// Switch the active warmer's economics to idle continuation estimates and
/// start its 30-minute age cap. The engine calls this when a run settles.
pub fn mark_idle(session_id: &str) {
    if let Some(warm) = registry().lock().unwrap().get(session_id) {
        warm.idle.store(true, Ordering::Relaxed);
    }
}

/// Whether a warmer task is currently registered for this conversation.
pub fn is_active(session_id: &str) -> bool {
    registry().lock().unwrap().contains_key(session_id)
}

/// Schedule (replacing any previous) a warmer that keeps the prompt-cache
/// entry written by the snapshot's request alive. No-op when replay is unsafe
/// or the TTL is too short to schedule.
pub fn schedule(
    client: Arc<dyn LLMClientTrait>,
    snapshot: WarmSnapshot,
    settings: PromptCacheWarmSettings,
) {
    if !settings.replay_safe {
        return;
    }
    let Some(delay) = warming_delay(Duration::from_secs(settings.ttl_secs)) else {
        return;
    };
    invalidate(&snapshot.session_id);
    let token = CancellationToken::new();
    let idle = Arc::new(AtomicBool::new(false));
    let generation = next_generation();
    registry().lock().unwrap().insert(
        snapshot.session_id.clone(),
        ActiveWarm {
            token: token.clone(),
            idle: Arc::clone(&idle),
            generation,
        },
    );
    tokio::spawn(warm_loop(
        client, snapshot, settings, delay, token, idle, generation,
    ));
}

async fn warm_loop(
    client: Arc<dyn LLMClientTrait>,
    snapshot: WarmSnapshot,
    settings: PromptCacheWarmSettings,
    delay: Duration,
    token: CancellationToken,
    idle: Arc<AtomicBool>,
    generation: u64,
) {
    let ttl = Duration::from_secs(settings.ttl_secs);
    // A timer can run late after sleep or event-loop blockage. Keep half of
    // the planned pre-expiry margin for that delay; a late refresh is likely a
    // full-price cache write, not a cache warm.
    let deadline_slack = (ttl - delay) / 2;
    let started_at = tokio::time::Instant::now();
    let mut next_fire = started_at + delay;

    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep_until(next_fire) => {}
        }
        if tokio::time::Instant::now() > next_fire + deadline_slack {
            tracing::debug!(session_id = %snapshot.session_id, "cache warm: refresh deadline missed");
            break;
        }

        let is_idle = idle.load(Ordering::Relaxed);
        let (decision, expected) = decide_warming(snapshot.prompt_tokens, is_idle, &settings);
        let CacheWarmDecision::Warm = decision else {
            tracing::debug!(
                session_id = %snapshot.session_id,
                idle = is_idle,
                expected_savings_usd = expected,
                reason = ?decision,
                "cache warm: stopping"
            );
            break;
        };

        // Replay the exact request with a one-token output cap. `maxRetries: 0`
        // in pi — thunder retries nothing here by construction.
        let options = ChatRequestOptions {
            messages: snapshot.messages.clone(),
            tools: snapshot.tools.clone(),
            model: Some(snapshot.model.clone()),
            temperature: snapshot.temperature,
            top_p: snapshot.top_p,
            max_tokens: Some(1),
            thinking_level: snapshot.thinking_level.clone(),
            // Refreshes are ordinary requests: default (short) retention keeps
            // writing/reading the same cache entry as the real turn.
            cache_retention: None,
            session_id: Some(snapshot.session_id.clone()),
        };
        match client.stream_chat(options, token.clone()).await {
            Ok(rx) => drain_replay(rx, &token).await,
            // Cache warming is best-effort and must not affect anything else.
            Err(err) => {
                tracing::warn!(session_id = %snapshot.session_id, error = %err, "cache warm: replay request failed")
            }
        }
        if token.is_cancelled() {
            return;
        }
        next_fire = tokio::time::Instant::now() + delay;
        if started_at.elapsed() > MAX_IDLE_WARMING_AGE {
            tracing::debug!(session_id = %snapshot.session_id, "cache warm: idle age limit reached");
            break;
        }
    }

    // Self-remove only when a newer schedule has not replaced us.
    let mut reg = registry().lock().unwrap();
    if reg
        .get(&snapshot.session_id)
        .is_some_and(|active| active.generation == generation)
    {
        reg.remove(&snapshot.session_id);
    }
}

async fn drain_replay(
    mut rx: mpsc::Receiver<Result<LLMStreamChunk, String>>,
    token: &CancellationToken,
) {
    while let Some(chunk) = rx.recv().await {
        match chunk {
            Ok(LLMStreamChunk::Completed {
                prompt_tokens,
                cached_tokens,
                ..
            }) => {
                tracing::info!(
                    prompt_tokens = ?prompt_tokens,
                    cache_read_tokens = ?cached_tokens,
                    "cache warmed: prompt-cache entry refreshed"
                );
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
        if token.is_cancelled() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sonnet_like() -> PromptCacheWarmSettings {
        PromptCacheWarmSettings {
            ttl_secs: 300,
            input_price_per_m: Some(3.0),
            cache_read_price_per_m: Some(0.3),
            cache_write_price_per_m: Some(3.75),
            replay_safe: true,
        }
    }

    #[test]
    fn warming_delay_preserves_margin() {
        // Too short to schedule.
        assert!(warming_delay(Duration::from_secs(10)).is_none());
        assert!(warming_delay(Duration::from_secs(5)).is_none());
        // 90% of TTL, but never closer than 10s to expiry.
        assert_eq!(
            warming_delay(Duration::from_secs(300)),
            Some(Duration::from_secs(270))
        );
        assert_eq!(
            warming_delay(Duration::from_secs(20)),
            Some(Duration::from_secs(10))
        );
        // 90% < ttl - 10s for long TTLs.
        assert_eq!(
            warming_delay(Duration::from_secs(3600)),
            Some(Duration::from_secs(3240))
        );
    }

    #[test]
    fn economics_follow_pi_semantics() {
        let s = sonnet_like();
        // Streaming phase (probability 1): 50k tokens of sonnet-like pricing.
        let (d, _) = decide_warming(50_000, false, &s);
        assert_eq!(d, CacheWarmDecision::Warm);
        // Same prompt while idle: 0.15 × $0.1725 − $0.015 < $0.05 → stop.
        let (d, expected) = decide_warming(50_000, true, &s);
        assert_eq!(
            d,
            CacheWarmDecision::Stop("expected savings below threshold")
        );
        assert!(expected < MIN_EXPECTED_SAVINGS_USD);
        // A huge prompt stays worth warming even idle.
        let (d, _) = decide_warming(500_000, true, &s);
        assert_eq!(d, CacheWarmDecision::Warm);
        // Missing pricing → economics unavailable, warming off.
        let mut no_price = sonnet_like();
        no_price.cache_write_price_per_m = None;
        let (d, _) = decide_warming(50_000, false, &no_price);
        assert_eq!(d, CacheWarmDecision::Stop("cache economics unavailable"));
        // No prompt tokens → stop.
        let (d, _) = decide_warming(0, false, &s);
        assert_eq!(d, CacheWarmDecision::Stop("no prompt tokens"));
    }
}
