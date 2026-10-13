//! Unified telemetry & token-speed plugin (`MetricsPlugin`).
//!
//! Computes live streaming token rates, token breakdowns, prompt-cache hit
//! rates, context-window occupancy, and wall-clock/decode durations as a
//! sidecar plugin outside `thunder-agent-loop`, so the core loop remains pure
//! while every host (`thunder-tui`, `thunder-daemon`, `agent-resume-panel`)
//! consumes one authoritative [`MetricsSnapshot`].

use crate::error::PluginError;
use crate::plugin::{PluginCapability, PluginContext, PluginManifest, ThunderPlugin, TriggerSpec};
use async_trait::async_trait;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use thunder_agent_loop::core::token_estimator::estimate_token_count;
use thunder_agent_loop::types::event::{AgentEvent, AgentStats, ObservedEvent, TurnStats};
use thunder_agent_loop::AgentRunResult;

/// Custom event `kind` emitted alongside `ObservedEvent` streams whenever the
/// metrics snapshot updates.
pub const METRICS_UPDATE_EVENT_KIND: &str = "metrics_update";

/// Unified model, token, speed, cache, context, and duration snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MetricsSnapshot {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    /// Whether the task is currently streaming / executing.
    pub is_running: bool,
    /// Current 1-based turn number in the run.
    pub turn: usize,

    // ── Speed (tokens / sec) ─────────────────────────────────────────────
    /// Live token generation rate during active streaming (excludes TTFT prefill
    /// wait and tool execution duration).
    pub live_tps: f64,
    /// Settled average token generation rate across completed turns of the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_tps: Option<f64>,

    // ── Live streaming counters ──────────────────────────────────────────
    /// Estimated total output tokens (answer + reasoning) streamed in this run.
    pub live_tokens: usize,
    /// Estimated reasoning tokens streamed in this run.
    pub live_reasoning_tokens: usize,

    // ── Turn / Run token breakdown ───────────────────────────────────────
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<usize>,

    // ── Prompt cache telemetry ───────────────────────────────────────────
    pub cached_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<usize>,
    /// `cached_tokens / prompt_tokens * 100.0` when `prompt_tokens > 0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_hit_percent: Option<f64>,

    // ── Context window & session cumulative usage ────────────────────────
    /// Current working context occupancy in tokens.
    pub context_tokens: usize,
    /// Model's maximum context window in tokens, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window_limit: Option<usize>,
    /// `context_tokens / context_window_limit * 100.0`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_usage_percent: Option<f64>,
    /// Cumulative billed tokens across all turns in this session.
    pub session_total_tokens: usize,

    // ── Timing breakdown ─────────────────────────────────────────────────
    /// Pure token decode duration (ms) accumulated across turns in this run.
    pub generation_duration_ms: u64,
    /// Tool execution duration (ms) accumulated across turns in this run.
    pub tool_duration_ms: u64,
    /// AgentLoop internal duration (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// End-to-end wall clock duration (ms) from task start to current/completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wall_duration_ms: Option<u64>,
}

/// Stateful per-run/session tracker behind [`MetricsPlugin`].
#[derive(Debug)]
pub struct SessionMetricsTracker {
    snapshot: MetricsSnapshot,
    run_started_at: Option<Instant>,
    turn_first_token_at: Option<Instant>,
    turn_gen_settled: bool,
    completed_gen_ms: u64,
    completed_tokens: usize,
    completed_reasoning: usize,
    turn_text: String,
    turn_reasoning: String,
    turn_text_tokens: usize,
    turn_reasoning_tokens: usize,
}

impl Default for SessionMetricsTracker {
    fn default() -> Self {
        Self::new("")
    }
}

impl SessionMetricsTracker {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            snapshot: MetricsSnapshot {
                session_id: session_id.into(),
                ..Default::default()
            },
            run_started_at: None,
            turn_first_token_at: None,
            turn_gen_settled: false,
            completed_gen_ms: 0,
            completed_tokens: 0,
            completed_reasoning: 0,
            turn_text: String::new(),
            turn_reasoning: String::new(),
            turn_text_tokens: 0,
            turn_reasoning_tokens: 0,
        }
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let mut snap = self.snapshot.clone();
        snap.live_tps = self.compute_live_tps();
        if let Some(started) = self.run_started_at {
            snap.wall_duration_ms = Some(started.elapsed().as_millis() as u64);
        }
        snap
    }

    /// Configure session-level baselines before or at run start.
    pub fn configure_session(
        &mut self,
        session_id: &str,
        model: Option<String>,
        thinking_level: Option<String>,
        context_window_limit: Option<usize>,
        initial_context_tokens: usize,
        initial_session_total_tokens: usize,
    ) {
        self.snapshot.session_id = session_id.to_string();
        if model.is_some() {
            self.snapshot.model = model;
        }
        if thinking_level.is_some() {
            self.snapshot.thinking_level = thinking_level;
        }
        if let Some(limit) = context_window_limit.filter(|&w| w > 0) {
            self.snapshot.context_window_limit = Some(limit);
        }
        if initial_context_tokens > 0 && self.snapshot.context_tokens == 0 {
            self.snapshot.context_tokens = initial_context_tokens;
        }
        if initial_session_total_tokens > self.snapshot.session_total_tokens {
            self.snapshot.session_total_tokens = initial_session_total_tokens;
        }
        self.recompute_percentages();
    }

    /// Start a new run while keeping the previous run's settled numbers
    /// (`avg_tps`, `cached_tokens`, `prompt_tokens`, etc.) until new turns
    /// replace them.
    pub fn begin_run(&mut self, task_id: Option<String>) {
        self.snapshot.task_id = task_id;
        self.snapshot.is_running = true;
        self.snapshot.turn = 0;
        self.snapshot.live_tps = 0.0;
        self.snapshot.live_tokens = 0;
        self.snapshot.live_reasoning_tokens = 0;
        self.snapshot.generation_duration_ms = 0;
        self.snapshot.tool_duration_ms = 0;
        self.snapshot.wall_duration_ms = None;
        self.run_started_at = Some(Instant::now());
        self.turn_first_token_at = None;
        self.turn_gen_settled = false;
        self.completed_gen_ms = 0;
        self.completed_tokens = 0;
        self.completed_reasoning = 0;
        self.turn_text.clear();
        self.turn_reasoning.clear();
        self.turn_text_tokens = 0;
        self.turn_reasoning_tokens = 0;
    }

    pub fn on_token_delta(&mut self, delta: &str) {
        self.turn_first_token_at.get_or_insert_with(Instant::now);
        self.turn_text.push_str(delta);
        self.turn_text_tokens = estimate_token_count(&self.turn_text);
        self.sync_live_counters();
    }

    pub fn on_reasoning_delta(&mut self, delta: &str) {
        self.turn_first_token_at.get_or_insert_with(Instant::now);
        self.turn_reasoning.push_str(delta);
        self.turn_reasoning_tokens = estimate_token_count(&self.turn_reasoning);
        self.sync_live_counters();
    }

    /// Pause the active generation clock when tool execution begins so tool
    /// latency never dilutes `live_tps`.
    pub fn pause_for_tools(&mut self) {
        if let Some(started) = self.turn_first_token_at.take() {
            let elapsed = (started.elapsed().as_millis() as u64).max(1);
            self.completed_gen_ms += elapsed;
            self.snapshot.generation_duration_ms = self.completed_gen_ms;
            self.turn_gen_settled = true;
        }
        self.sync_live_counters();
    }

    pub fn on_turn_end(&mut self, stats: &TurnStats) {
        if let Some(started) = self.turn_first_token_at.take() {
            let elapsed = (started.elapsed().as_millis() as u64).max(1);
            self.completed_gen_ms += elapsed;
        } else if !self.turn_gen_settled {
            let ct = stats.completion_tokens.unwrap_or(0);
            if let Some(tps) = stats.tokens_per_second.filter(|&t| t > 0.0) {
                if ct > 0 {
                    let ms = ((ct as f64 / tps) * 1000.0).round() as u64;
                    self.completed_gen_ms += ms.max(1);
                }
            } else if stats.duration_ms > 0 && ct > 0 {
                self.completed_gen_ms += stats.duration_ms;
            }
        }
        self.snapshot.generation_duration_ms = self.completed_gen_ms;

        let turn_comp = stats
            .completion_tokens
            .filter(|&c| c > 0)
            .unwrap_or(self.turn_text_tokens + self.turn_reasoning_tokens);
        let turn_reas = stats
            .reasoning_tokens
            .filter(|&r| r > 0)
            .unwrap_or(self.turn_reasoning_tokens);

        self.completed_tokens += turn_comp;
        self.completed_reasoning += turn_reas;
        self.turn_text.clear();
        self.turn_reasoning.clear();
        self.turn_text_tokens = 0;
        self.turn_reasoning_tokens = 0;
        self.turn_gen_settled = false;

        let pt = stats.prompt_tokens;
        let ct = stats.completion_tokens.or(Some(turn_comp));
        let turn_total = match (pt, ct) {
            (None, None) => None,
            (p, c) => Some(p.unwrap_or(0) + c.unwrap_or(0)),
        };

        self.snapshot.turn = stats.turn;
        self.snapshot.prompt_tokens = pt;
        self.snapshot.completion_tokens = ct;
        self.snapshot.reasoning_tokens = stats.reasoning_tokens.or(Some(turn_reas));
        self.snapshot.total_tokens = turn_total;
        if let Some(cached) = stats.cached_tokens {
            self.snapshot.cached_tokens = cached;
        } else if pt.is_some() {
            self.snapshot.cached_tokens = 0;
        }
        self.snapshot.cache_write_tokens = stats.cache_write_tokens;
        self.snapshot.duration_ms = Some(stats.duration_ms);

        let tps = stats
            .tokens_per_second
            .filter(|t| t.is_finite() && *t > 0.0)
            .or_else(|| {
                if self.completed_gen_ms > 0 && self.completed_tokens > 0 {
                    Some(self.completed_tokens as f64 / (self.completed_gen_ms as f64 / 1000.0))
                } else {
                    None
                }
            });
        self.snapshot.avg_tps = tps;

        if let Some(total) = turn_total.filter(|&t| t > 0) {
            self.snapshot.context_tokens = total;
            self.snapshot.session_total_tokens =
                self.snapshot.session_total_tokens.saturating_add(total);
        }

        self.sync_live_counters();
        self.recompute_percentages();
    }

    pub fn on_finish(&mut self, stats: Option<&AgentStats>) {
        self.snapshot.is_running = false;
        if let Some(started) = self.turn_first_token_at.take() {
            let elapsed = (started.elapsed().as_millis() as u64).max(1);
            self.completed_gen_ms += elapsed;
            self.snapshot.generation_duration_ms = self.completed_gen_ms;
        }
        if let Some(run_start) = self.run_started_at.take() {
            self.snapshot.wall_duration_ms = Some(run_start.elapsed().as_millis() as u64);
        }

        if let Some(s) = stats {
            let total = s.total_prompt_tokens + s.total_completion_tokens;
            self.snapshot.prompt_tokens = Some(s.total_prompt_tokens);
            self.snapshot.completion_tokens = Some(s.total_completion_tokens);
            self.snapshot.reasoning_tokens = Some(s.total_reasoning_tokens);
            self.snapshot.total_tokens = Some(total);
            self.snapshot.cached_tokens = s.total_cached_tokens;
            self.snapshot.tool_duration_ms = s.total_tool_time_ms;
            self.snapshot.duration_ms = Some(s.total_duration_ms);
            if self.snapshot.wall_duration_ms.is_none() {
                self.snapshot.wall_duration_ms = Some(s.total_duration_ms);
            }
            let tps = s
                .avg_tokens_per_second
                .filter(|t| t.is_finite() && *t > 0.0)
                .or_else(|| {
                    if self.completed_gen_ms > 0 && s.total_completion_tokens > 0 {
                        Some(
                            s.total_completion_tokens as f64
                                / (self.completed_gen_ms as f64 / 1000.0),
                        )
                    } else {
                        None
                    }
                });
            self.snapshot.avg_tps = tps;
            if let Some(avg) = tps {
                self.snapshot.live_tps = avg;
            }
        }
        self.recompute_percentages();
    }

    pub fn apply_event(&mut self, event: &AgentEvent) -> bool {
        match event {
            AgentEvent::TurnStart { turn, .. } => {
                self.snapshot.turn = *turn;
                self.snapshot.is_running = true;
                self.turn_first_token_at = None;
                self.turn_gen_settled = false;
                true
            }
            AgentEvent::TokenDelta { delta, .. } => {
                self.on_token_delta(delta);
                true
            }
            AgentEvent::ReasoningDelta { delta, .. } => {
                self.on_reasoning_delta(delta);
                true
            }
            AgentEvent::ToolExecStart { .. } => {
                self.pause_for_tools();
                true
            }
            AgentEvent::ToolExecResult { result, .. } => {
                self.snapshot.tool_duration_ms = self
                    .snapshot
                    .tool_duration_ms
                    .saturating_add(result.duration_ms);
                true
            }
            AgentEvent::ContextCompacted { tokens_after, .. } => {
                self.snapshot.context_tokens = *tokens_after;
                self.recompute_percentages();
                true
            }
            AgentEvent::TurnEnd { stats, .. } => {
                self.on_turn_end(stats);
                true
            }
            AgentEvent::LoopComplete { stats, .. } => {
                self.on_finish(Some(stats));
                true
            }
            _ => false,
        }
    }

    fn sync_live_counters(&mut self) {
        self.snapshot.live_tokens =
            self.completed_tokens + self.turn_text_tokens + self.turn_reasoning_tokens;
        self.snapshot.live_reasoning_tokens = self.completed_reasoning + self.turn_reasoning_tokens;
        self.snapshot.live_tps = self.compute_live_tps();
        if let Some(started) = self.run_started_at {
            self.snapshot.wall_duration_ms = Some(started.elapsed().as_millis() as u64);
        }
    }

    fn compute_live_tps(&self) -> f64 {
        if self.snapshot.live_tokens == 0 {
            return 0.0;
        }
        let current_ms = self
            .turn_first_token_at
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        let total_gen_sec = (self.completed_gen_ms + current_ms) as f64 / 1000.0;
        if total_gen_sec <= 0.2 {
            return 0.0;
        }
        self.snapshot.live_tokens as f64 / total_gen_sec
    }

    fn recompute_percentages(&mut self) {
        let prompt = self.snapshot.prompt_tokens.unwrap_or(0);
        let cached = self.snapshot.cached_tokens;
        self.snapshot.cache_hit_percent = if prompt > 0 && cached > 0 {
            Some((cached as f64 / prompt as f64) * 100.0)
        } else {
            None
        };

        self.snapshot.context_usage_percent = self
            .snapshot
            .context_window_limit
            .filter(|&limit| limit > 0)
            .map(|limit| {
                ((self.snapshot.context_tokens as f64 / limit as f64) * 100.0).clamp(0.0, 100.0)
            });
    }
}

/// First-class telemetry plugin that tracks token rates, context occupancy,
/// prompt cache hits, and wall-clock/decode timings without touching the core
/// agent loop.
#[derive(Clone)]
pub struct MetricsPlugin {
    manifest: PluginManifest,
    sessions: Arc<RwLock<HashMap<String, SessionMetricsTracker>>>,
}

impl Default for MetricsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MetricsPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsPlugin")
            .field("id", &self.manifest.id)
            .finish()
    }
}

impl MetricsPlugin {
    pub fn new() -> Self {
        let manifest = PluginManifest::new(
            "metrics",
            "Thunder Telemetry & Token Speed Tracker",
            "Tracks live streaming token rates, context window occupancy, prompt cache hit rates, and turn/run durations.",
            "0.1.0",
        )
        .with_capability(PluginCapability::Custom("telemetry".to_string()))
        .with_triggers(TriggerSpec::always());

        Self {
            manifest,
            sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Pre-seed or update session metadata (model, context window limit,
    /// existing context tokens, cumulative session tokens).
    pub fn configure_session(
        &self,
        session_id: &str,
        model: Option<String>,
        thinking_level: Option<String>,
        context_window_limit: Option<usize>,
        initial_context_tokens: usize,
        initial_session_total_tokens: usize,
    ) {
        let mut guard = self.sessions.write();
        let tracker = guard
            .entry(session_id.to_string())
            .or_insert_with(|| SessionMetricsTracker::new(session_id));
        tracker.configure_session(
            session_id,
            model,
            thinking_level,
            context_window_limit,
            initial_context_tokens,
            initial_session_total_tokens,
        );
    }

    /// Read the latest [`MetricsSnapshot`] for `session_id`.
    pub fn snapshot(&self, session_id: &str) -> Option<MetricsSnapshot> {
        let guard = self.sessions.read();
        guard.get(session_id).map(SessionMetricsTracker::snapshot)
    }

    /// Record an event directly for `session_id` and return the updated
    /// snapshot if the event affected metrics.
    pub fn record_session_event(
        &self,
        session_id: &str,
        event: &AgentEvent,
    ) -> Option<MetricsSnapshot> {
        let mut guard = self.sessions.write();
        let tracker = guard
            .entry(session_id.to_string())
            .or_insert_with(|| SessionMetricsTracker::new(session_id));
        if tracker.apply_event(event) {
            Some(tracker.snapshot())
        } else {
            None
        }
    }
}

#[async_trait]
impl ThunderPlugin for MetricsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    async fn on_init(&self, ctx: &PluginContext) -> Result<(), PluginError> {
        let mut guard = self.sessions.write();
        let tracker = guard
            .entry(ctx.session_id.clone())
            .or_insert_with(|| SessionMetricsTracker::new(&ctx.session_id));
        tracker.begin_run(ctx.route.clone());
        Ok(())
    }

    async fn on_event(&self, event: &ObservedEvent, ctx: &PluginContext) {
        let _ = self.record_session_event(&ctx.session_id, &event.event);
    }

    async fn on_finish(
        &self,
        result: &AgentRunResult,
        ctx: &PluginContext,
    ) -> Result<(), PluginError> {
        let mut guard = self.sessions.write();
        let tracker = guard
            .entry(ctx.session_id.clone())
            .or_insert_with(|| SessionMetricsTracker::new(&ctx.session_id));
        tracker.on_finish(Some(&result.stats));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_computes_live_tps_and_excludes_tool_wait() {
        let mut tracker = SessionMetricsTracker::new("sess_1");
        tracker.configure_session(
            "sess_1",
            Some("openai/gpt-4o".to_string()),
            Some("high".to_string()),
            Some(128_000),
            1_000,
            5_000,
        );
        tracker.begin_run(Some("task_1".to_string()));
        tracker.on_token_delta("这是一段流式生成的中文测试内容，包含多个词组和标点符号。");
        tracker.on_reasoning_delta("思考过程推理分析");

        let snap = tracker.snapshot();
        assert!(snap.live_tokens > 20);
        assert!(snap.live_reasoning_tokens > 5);
        assert_eq!(snap.context_window_limit, Some(128_000));

        // Pause for tools and finish turn 1
        tracker.pause_for_tools();
        tracker.on_turn_end(&TurnStats {
            turn: 1,
            prompt_tokens: Some(4_000),
            completion_tokens: Some(200),
            cached_tokens: Some(3_000),
            cache_write_tokens: None,
            reasoning_tokens: Some(50),
            duration_ms: 2_500,
            tool_calls_count: 1,
            tokens_per_second: Some(80.0),
        });

        let after_turn = tracker.snapshot();
        assert_eq!(after_turn.prompt_tokens, Some(4_000));
        assert_eq!(after_turn.completion_tokens, Some(200));
        assert_eq!(after_turn.cached_tokens, 3_000);
        assert_eq!(after_turn.cache_hit_percent, Some(75.0));
        assert_eq!(after_turn.context_tokens, 4_200);
        assert_eq!(after_turn.session_total_tokens, 5_000 + 4_200);
        assert_eq!(after_turn.avg_tps, Some(80.0));
    }
}
