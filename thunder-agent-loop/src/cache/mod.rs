//! Prompt-cache lifecycle management (warming).

pub mod warmer;

pub use warmer::{
    decide_warming, invalidate, is_active, mark_idle, schedule, warming_delay, CacheWarmDecision,
    PromptCacheWarmSettings, WarmSnapshot, IDLE_CONTINUATION_PROBABILITY, MAX_IDLE_WARMING_AGE,
    MIN_EXPECTED_SAVINGS_USD,
};
