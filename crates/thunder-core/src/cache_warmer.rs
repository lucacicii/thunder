//! Prompt-cache warming ported from `main`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use thunder_protocol::{
    ChatMessage, ChatRequestOptions, LLMClientTrait, LLMStreamChunk, PromptCacheWarmSettings,
    ToolDefinition,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const IDLE_CONTINUATION_PROBABILITY: f64 = 0.15;
pub const MIN_EXPECTED_SAVINGS_USD: f64 = 0.05;
pub const MAX_IDLE_WARMING_AGE: Duration = Duration::from_secs(30 * 60);

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

#[derive(Debug, Clone)]
pub struct WarmSnapshot {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub thinking_level: Option<String>,
    pub session_id: String,
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

pub fn invalidate(session_id: &str) {
    if let Some(warm) = registry().lock().unwrap().remove(session_id) {
        warm.token.cancel();
    }
}

pub fn mark_idle(session_id: &str) {
    if let Some(warm) = registry().lock().unwrap().get(session_id) {
        warm.idle.store(true, Ordering::Relaxed);
    }
}

pub fn is_active(session_id: &str) -> bool {
    registry().lock().unwrap().contains_key(session_id)
}

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
    let deadline_slack = (ttl - delay) / 2;
    let started_at = tokio::time::Instant::now();
    let mut next_fire = started_at + delay;

    loop {
        tokio::select! {
            _ = token.cancelled() => return,
            _ = tokio::time::sleep_until(next_fire) => {}
        }
        if tokio::time::Instant::now() > next_fire + deadline_slack {
            break;
        }

        let is_idle = idle.load(Ordering::Relaxed);
        let (decision, _) = decide_warming(snapshot.prompt_tokens, is_idle, &settings);
        let CacheWarmDecision::Warm = decision else {
            break;
        };

        let options = ChatRequestOptions {
            messages: snapshot.messages.clone(),
            tools: snapshot.tools.clone(),
            model: Some(snapshot.model.clone()),
            temperature: snapshot.temperature,
            top_p: snapshot.top_p,
            max_tokens: Some(1),
            thinking_level: snapshot.thinking_level.clone(),
            cache_retention: None,
            session_id: Some(snapshot.session_id.clone()),
        };
        if let Ok(rx) = client.stream_chat(options, token.clone()).await {
            drain_replay(rx, &token).await;
        }
        if token.is_cancelled() {
            return;
        }
        next_fire = tokio::time::Instant::now() + delay;
        if started_at.elapsed() > MAX_IDLE_WARMING_AGE {
            break;
        }
    }

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
            Ok(LLMStreamChunk::Completed { .. }) | Err(_) => break,
            Ok(_) => {}
        }
        if token.is_cancelled() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn economics_gate_evaluates_expected_savings() {
        let s = PromptCacheWarmSettings {
            ttl_secs: 300,
            input_price_per_m: Some(3.0),
            cache_read_price_per_m: Some(0.3),
            cache_write_price_per_m: Some(3.75),
            replay_safe: true,
        };
        let (d, _) = decide_warming(50_000, false, &s);
        assert_eq!(d, CacheWarmDecision::Warm);
        let (d_idle, _) = decide_warming(50_000, true, &s);
        assert_eq!(
            d_idle,
            CacheWarmDecision::Stop("expected savings below threshold")
        );
    }
}
