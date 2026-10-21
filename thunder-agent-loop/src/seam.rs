//! The kernel's tool seam.
//!
//! This module is the **only** thing the loop knows about tools:
//!
//! ```text
//! loop --specs()--> host       每轮一次，顺序稳定
//! loop --call()---> host       可并行；超时/取消由内核施加
//! ```
//!
//! Everything else — approval, dangerous-command policy, path scoping, audit
//! traces, MCP transport, sandboxing — happens *inside* a `Tools`
//! implementation, i.e. in the host. The loop never asks "may I?"; it asks
//! "what can I call?" and "please call this".
//!
//! This module is deliberately free of every policy type. The reference
//! adapter lives in [`crate::tools::registry_tools`]; a host that wants its own
//! policy supplies its own `Tools` through
//! [`AgentLoop::with_tools`](crate::loop_engine::engine::AgentLoop::with_tools).
//!
//! `thunder-agent-loop/tests/kernel_boundary_test.rs` asserts that this file
//! never mentions a policy type, so the boundary cannot erode by accident.

use crate::tools::notice::SystemNotice;
use crate::types::message::ToolCall;
use crate::types::tool::{ToolDefinition, ToolEventSink, ToolExecutionResult};
use futures_util::future::BoxFuture;
use futures_util::future::join_all;
use std::sync::Arc;
use thunder_agent_types::trace::ToolTraceSink;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// How a tool may be scheduled relative to its batch peers.
///
/// This is a *scheduling* declaration, not a permission one: a single-writer
/// tool, a serial port, or a file-locking editor says `Sequential` so the loop
/// stops running it alongside its siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Concurrency {
    /// May run alongside every other parallel call in the batch.
    #[default]
    Parallel,
    /// Any batch containing this tool runs sequentially, in `tool_calls` order.
    Sequential,
}

/// One callable tool, as the loop sees it.
///
/// Two fields, by design: a definition the model is shown, and how it may be
/// scheduled. There is deliberately no `effect`, no `read_only`, no `risk` —
/// the loop must not grow an opinion about what a call *means*.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub definition: ToolDefinition,
    pub concurrency: Concurrency,
}

impl ToolSpec {
    pub fn new(definition: ToolDefinition, concurrency: Concurrency) -> Self {
        Self {
            definition,
            concurrency,
        }
    }

    /// Convenience for the common case.
    pub fn parallel(definition: ToolDefinition) -> Self {
        Self::new(definition, Concurrency::Parallel)
    }
}

/// The model-visible result of one call.
///
/// No decision, no approval answer, no timing breakdown: the loop times the
/// call itself, and everything a host wants to audit it recorded on its own
/// side while producing this. `truncated` / `original_bytes` ride along because
/// the host's truncation is a fact about the *content* it chose to hand back.
#[derive(Debug, Clone, Default)]
pub struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
    pub truncated: bool,
    pub original_bytes: usize,
    /// A structured, model-facing note the host attached to this outcome
    /// (refusal, atomic write, self-healing recovery). Rendered into the tool
    /// message by the provider layer, exactly as before.
    pub telemetry: Option<SystemNotice>,
}

impl ToolOutcome {
    pub fn success(output: impl Into<String>) -> Self {
        let output = output.into();
        Self {
            original_bytes: output.len(),
            output,
            ..Default::default()
        }
    }

    pub fn error(reason: impl Into<String>) -> Self {
        let output = reason.into();
        Self {
            original_bytes: output.len(),
            output,
            is_error: true,
            ..Default::default()
        }
    }
}

impl From<ToolExecutionResult> for ToolOutcome {
    fn from(value: ToolExecutionResult) -> Self {
        Self {
            output: value.output,
            is_error: value.is_error,
            truncated: value.truncated,
            original_bytes: value.original_bytes,
            telemetry: value.telemetry,
        }
    }
}

impl From<ToolOutcome> for ToolExecutionResult {
    fn from(value: ToolOutcome) -> Self {
        Self {
            output: value.output,
            is_error: value.is_error,
            truncated: value.truncated,
            original_bytes: value.original_bytes,
            duration_ms: 0,
            telemetry: value.telemetry,
        }
    }
}

/// What the loop passes down with one call.
///
/// Borrowed, so a batch costs no per-call allocation beyond the call itself.
pub struct ToolCallContext<'a> {
    pub tool_call_id: &'a str,
    pub turn: usize,
    pub route: Option<&'a str>,
    pub cancel: &'a CancellationToken,
    pub timeout: Option<Duration>,
    /// The run's business-event sink, when the kernel has one.
    ///
    /// The loop owns the *run's* stream, so it is the only party that can hand a
    /// tool the channel its non-model events (a file change, a validation
    /// failure) belong on. Opaque by design: the kernel never interprets it.
    pub events: Option<Arc<dyn ToolEventSink>>,
}

/// The host's tools, as the loop sees them.
///
/// Contract for implementors:
///
/// * `specs()` is called **once per turn**; the order must be stable across
///   calls, because the list is part of the provider request prefix.
/// * `call()` must not panic, must observe `ctx.cancel`, and must turn every
///   failure into `ToolOutcome { is_error: true, .. }`. A returned error is
///   fed back to the model; it does not end the run.
pub trait Tools: Send + Sync {
    fn specs(&self) -> Vec<ToolSpec>;

    fn call<'a>(
        &'a self,
        call: &'a ToolCall,
        ctx: ToolCallContext<'a>,
    ) -> BoxFuture<'a, ToolOutcome>;
}

/// One settled call: the call the model asked for and the result the seam
/// produced for it. Kept here (rather than reusing the executor's pair type) so
/// the seam's signatures never mention a policy type.
#[derive(Debug, Clone)]
pub struct ExecutedCall {
    pub tool_call: ToolCall,
    pub result: ToolExecutionResult,
}


/// What the loop hands a factory for one run.
///
/// The run's sinks are created by the loop (it owns the run's stream), so a host
/// that wants per-run observability receives them here rather than reaching into
/// the loop.
pub struct RunContext {
    pub session_id: Option<String>,
    pub route: Option<String>,
    pub events: Arc<dyn ToolEventSink>,
    pub trace: Arc<dyn ToolTraceSink>,
}

impl std::fmt::Debug for RunContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunContext")
            .field("session_id", &self.session_id)
            .field("route", &self.route)
            .finish_non_exhaustive()
    }
}

/// Builds the tools for one run.
///
/// Use this when tool execution needs the run's event channel or trace sink —
/// i.e. whenever the host wants tool traces. A host that needs neither can use
/// [`Tools`] directly through `AgentLoop::with_tools`.
pub trait ToolsFactory: Send + Sync {
    fn for_run(&self, run: &RunContext) -> Arc<dyn Tools>;
}

/// A unit with no tools at all: the model is asked without declarations, and a
/// call that nevertheless names one is answered with an error instead of
/// failing the run.
#[derive(Debug, Default)]
pub struct NoTools;

impl Tools for NoTools {
    fn specs(&self) -> Vec<ToolSpec> {
        Vec::new()
    }

    fn call<'a>(
        &'a self,
        call: &'a ToolCall,
        _ctx: ToolCallContext<'a>,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            ToolOutcome::error(format!("no tool named '{}'", call.function.name))
        })
    }
}

/// Run one batch of tool calls through the seam.
///
/// Scheduling rule: if **any** call in the batch declares
/// [`Concurrency::Sequential`], the whole batch runs in `tool_calls` order;
/// otherwise the batch is fanned out with `join_all`.
///
/// Pairing rule: the returned vector is always the same length as `calls`, in
/// the same order — including when the run is cancelled mid-batch. A provider
/// rejects a request whose `tool_calls` and tool messages do not pair, so a
/// missing result is not an option.
pub async fn execute_batch(
    tools: &dyn Tools,
    calls: &[ToolCall],
    turn: usize,
    route: Option<&str>,
    cancel: &CancellationToken,
    timeout: Option<Duration>,
    events: Option<Arc<dyn ToolEventSink>>,
    sequential: bool,
) -> Vec<ExecutedCall> {
    if calls.is_empty() {
        return Vec::new();
    }

    if sequential {
        let mut out = Vec::with_capacity(calls.len());
        for call in calls {
            out.push(run_one(tools, call, turn, route, cancel, timeout, events.clone()).await);
        }
        return out;
    }

    let futures = calls.iter().map(|call| {
        run_one(tools, call, turn, route, cancel, timeout, events.clone())
    });
    join_all(futures).await
}

async fn run_one(
    tools: &dyn Tools,
    call: &ToolCall,
    turn: usize,
    route: Option<&str>,
    cancel: &CancellationToken,
    timeout: Option<Duration>,
    events: Option<Arc<dyn ToolEventSink>>,
) -> ExecutedCall {
    // A cancelled run still owes a result for every call it announced. The
    // seam is not asked to execute in that case; the refusal is generated here.
    if cancel.is_cancelled() {
        return ExecutedCall {
            tool_call: call.clone(),
            result: ToolExecutionResult::error("Tool execution cancelled".to_string(), Duration::ZERO),
        };
    }

    let started = Instant::now();
    let ctx = ToolCallContext {
        tool_call_id: &call.id,
        turn,
        route,
        cancel,
        timeout,
        events,
    };
    let outcome = tools.call(call, ctx).await;
    let mut result: ToolExecutionResult = outcome.into();
    result.duration_ms = started.elapsed().as_millis() as u64;
    ExecutedCall {
        tool_call: call.clone(),
        result,
    }
}

/// Batch scheduling decision: does this batch have to run in order?
pub fn batch_is_sequential(specs: &[ToolSpec], calls: &[ToolCall]) -> bool {
    calls.iter().any(|call| {
        specs
            .iter()
            .find(|spec| spec.definition.function.name == call.function.name)
            .map(|spec| spec.concurrency == Concurrency::Sequential)
            .unwrap_or(false)
    })
}

/// Map a spec list to the wire definitions a provider request carries.
pub fn definitions(specs: &[ToolSpec]) -> Vec<ToolDefinition> {
    specs
        .iter()
        .map(|spec| spec.definition.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct StubTools {
        specs: Vec<ToolSpec>,
        calls: Mutex<Vec<String>>,
        order: Mutex<Vec<String>>,
        specs_calls: AtomicUsize,
        sequential: bool,
    }

    impl StubTools {
        fn new(names: &[(&str, Concurrency)]) -> Arc<Self> {
            Arc::new(Self {
                specs: names
                    .iter()
                    .map(|(n, c)| ToolSpec::new(ToolDefinition::new_function(*n, "stub", serde_json::json!({})), *c))
                    .collect(),
                calls: Mutex::new(Vec::new()),
                order: Mutex::new(Vec::new()),
                specs_calls: AtomicUsize::new(0),
                sequential: names.iter().any(|(_, c)| *c == Concurrency::Sequential),
            })
        }
    }

    impl Tools for StubTools {
        fn specs(&self) -> Vec<ToolSpec> {
            self.specs_calls.fetch_add(1, Ordering::Relaxed);
            self.specs.clone()
        }

        fn call<'a>(
            &'a self,
            call: &'a ToolCall,
            _ctx: ToolCallContext<'a>,
        ) -> BoxFuture<'a, ToolOutcome> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(call.function.name.clone());
                self.order.lock().unwrap().push(call.function.name.clone());
                if self.sequential {
                    // Give a parallel sibling a chance to interleave.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                ToolOutcome::success(format!("ran {}", call.function.name))
            })
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall::new_function(id, name, "{}")
    }

    #[tokio::test]
    async fn specs_are_stable_and_ordered() {
        let tools = StubTools::new(&[("a", Concurrency::Parallel), ("b", Concurrency::Parallel)]);
        let first: Vec<String> = tools.specs().into_iter().map(|s| s.definition.function.name).collect();
        let second: Vec<String> = tools.specs().into_iter().map(|s| s.definition.function.name).collect();
        assert_eq!(first, second);
        assert_eq!(first, vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn batch_pairs_every_call_in_order() {
        let tools = StubTools::new(&[("a", Concurrency::Parallel), ("b", Concurrency::Parallel)]);
        let calls = vec![call("1", "a"), call("2", "b")];
        let specs = tools.specs();
        let out = execute_batch(
            tools.as_ref(),
            &calls,
            1,
            None,
            &CancellationToken::new(),
            None,
            None,
            batch_is_sequential(&specs, &calls),
        )
        .await;
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tool_call.id, "1");
        assert_eq!(out[1].tool_call.id, "2");
        assert!(!out[0].result.is_error);
    }

    #[tokio::test]
    async fn a_sequential_tool_forces_the_whole_batch_in_order() {
        let tools = StubTools::new(&[("a", Concurrency::Sequential), ("b", Concurrency::Parallel)]);
        let calls = vec![call("1", "a"), call("2", "b")];
        let specs = tools.specs();
        assert!(batch_is_sequential(&specs, &calls));
        let out = execute_batch(
            tools.as_ref(),
            &calls,
            1,
            None,
            &CancellationToken::new(),
            None,
            None,
            true,
        )
        .await;
        assert_eq!(out.len(), 2);
        assert_eq!(*tools.order.lock().unwrap(), vec!["a".to_string(), "b".to_string()]);
    }

    #[tokio::test]
    async fn a_cancelled_batch_still_answers_every_call() {
        let tools = StubTools::new(&[("a", Concurrency::Parallel)]);
        let calls = vec![call("1", "a"), call("2", "a")];
        let token = CancellationToken::new();
        token.cancel();
        let out = execute_batch(tools.as_ref(), &calls, 1, None, &token, None, None, false).await;
        assert_eq!(out.len(), 2, "pairing invariant");
        assert!(out.iter().all(|r| r.result.is_error));
        assert!(tools.calls.lock().unwrap().is_empty(), "nothing ran");
    }

    #[tokio::test]
    async fn an_unknown_tool_name_does_not_stop_the_batch() {
        let tools = StubTools::new(&[("a", Concurrency::Parallel)]);
        let calls = vec![call("1", "missing")];
        // The stub answers anything; the real RegistryTools returns an error
        // outcome. What matters here is that the seam produces a paired result.
        let out = execute_batch(
            tools.as_ref(),
            &calls,
            1,
            None,
            &CancellationToken::new(),
            None,
            None,
            false,
        )
        .await;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tool_call.id, "1");
    }

    #[test]
    fn outcome_round_trips_through_the_legacy_result() {
        let outcome = ToolOutcome {
            output: "oops".into(),
            is_error: true,
            truncated: true,
            original_bytes: 12,
            telemetry: None,
        };
        let legacy: ToolExecutionResult = outcome.clone().into();
        let back: ToolOutcome = legacy.into();
        assert_eq!(back.output, outcome.output);
        assert_eq!(back.is_error, outcome.is_error);
        assert_eq!(back.truncated, outcome.truncated);
        assert_eq!(back.original_bytes, outcome.original_bytes);
    }
}
