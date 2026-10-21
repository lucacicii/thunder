use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thunder_agent_loop::types::message::ToolCall;
use thunder_agent_policy::executor::ToolExecutor;
use thunder_agent_policy::plugin::{
    CallToolResult, InterceptorPlugin, Plugin, PreExecutionVerdict, ToolPlugin,
};
use thunder_agent_policy::registry::ToolRegistry;
use thunder_agent_policy::ToolExecutionContext;
use thunder_agent_types::tool::ToolDefinition;

fn executor_with(
    plugins: Vec<Arc<dyn ToolPlugin>>,
    interceptors: Vec<Arc<dyn InterceptorPlugin>>,
) -> ToolExecutor {
    let mut registry = ToolRegistry::new(64 * 1024, std::time::Duration::from_millis(60_000));
    for plugin in plugins {
        registry.register_plugin(plugin);
    }
    let mut executor = ToolExecutor::new(registry);
    for interceptor in interceptors {
        executor = executor.with_interceptor(interceptor);
    }
    executor
}

// Test Plugin A (Leaf Tool)
struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn id(&self) -> &'static str {
        "echo"
    }
}

#[async_trait]
impl ToolPlugin for EchoPlugin {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "echo",
            "Echoes input text",
            json!({
                "type": "object",
                "properties": { "msg": { "type": "string" } },
                "required": ["msg"]
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        let msg = args.get("msg").and_then(|v| v.as_str()).unwrap_or("");
        Ok(CallToolResult::success_text(format!("Echo: {msg}")))
    }
}

// Test Plugin B (Calls EchoPlugin via ctx.invoke)
struct ForwardPlugin;

impl Plugin for ForwardPlugin {
    fn id(&self) -> &'static str {
        "forward"
    }
}

#[async_trait]
impl ToolPlugin for ForwardPlugin {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            "forward",
            "Forwards text to echo tool",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } }
            }),
        )
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        // Invoke echo via safe context proxy
        let sub_res = ctx.invoke("echo", json!({ "msg": text })).await?;
        Ok(CallToolResult::success_text(format!(
            "Forwarded [{}]",
            sub_res.plain_text()
        )))
    }
}

// Test Plugin C (Recursive self caller to test cycle detection)
struct CyclePlugin;

impl Plugin for CyclePlugin {
    fn id(&self) -> &'static str {
        "cycle"
    }
}

#[async_trait]
impl ToolPlugin for CyclePlugin {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function("cycle", "Recursively calls itself", json!({}))
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<CallToolResult, String> {
        // Attempt recursive call
        ctx.invoke("cycle", json!({})).await
    }
}

// Test Interceptor
struct TestAuditInterceptor {
    pre_count: Arc<AtomicUsize>,
    post_count: Arc<AtomicUsize>,
    deny_tool: Option<String>,
}

impl Plugin for TestAuditInterceptor {
    fn id(&self) -> &'static str {
        "test_audit"
    }
}

#[async_trait]
impl InterceptorPlugin for TestAuditInterceptor {
    async fn pre_execute(
        &self,
        tool: &str,
        _args: &serde_json::Value,
        _ctx: &ToolExecutionContext,
    ) -> Result<PreExecutionVerdict, String> {
        self.pre_count.fetch_add(1, Ordering::SeqCst);
        if let Some(ref d) = self.deny_tool {
            if tool == d {
                return Ok(PreExecutionVerdict::Deny {
                    reason: format!("Forbidden tool '{tool}' blocked by interceptor"),
                });
            }
        }
        Ok(PreExecutionVerdict::Allow)
    }

    async fn post_execute(
        &self,
        _tool: &str,
        _args: &serde_json::Value,
        _result: &mut CallToolResult,
        _ctx: &ToolExecutionContext,
    ) {
        self.post_count.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn test_tool_plugin_registration_and_execution() {
    let executor = executor_with(vec![Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>], vec![]);
    let call = ToolCall::new_function("call_1", "echo", json!({ "msg": "hello mcp" }).to_string());
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(!res.is_error);
    assert_eq!(res.output, "Echo: hello mcp");
}

#[tokio::test]
async fn test_sub_tool_invocation_via_context() {
    let executor = executor_with(
        vec![
            Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>,
            Arc::new(ForwardPlugin) as Arc<dyn ToolPlugin>,
        ],
        vec![],
    );
    let call = ToolCall::new_function(
        "call_2",
        "forward",
        json!({ "text": "deep call" }).to_string(),
    );
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(!res.is_error);
    assert_eq!(res.output, "Forwarded [Echo: deep call]");
}

#[tokio::test]
async fn test_cycle_detection_prevents_deadlock() {
    let executor = executor_with(vec![Arc::new(CyclePlugin) as Arc<dyn ToolPlugin>], vec![]);
    let call = ToolCall::new_function("call_3", "cycle", json!({}).to_string());
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(res.is_error);
    assert!(
        res.output.contains("Cycle detected"),
        "Expected cycle detection error, got: {}",
        res.output
    );
}

#[tokio::test]
async fn test_max_depth_prevents_deep_recursion() {
    let executor = executor_with(
        vec![
            Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>,
            Arc::new(ForwardPlugin) as Arc<dyn ToolPlugin>,
        ],
        vec![],
    );
    let call = ToolCall::new_function("call_4", "forward", json!({ "text": "hi" }).to_string());

    // Set max_depth to 0, which should forbid any sub-invocation
    let ctx = ToolExecutionContext::default().with_max_depth(0);
    let res = executor.run_tool(&call, ctx, None).await;

    assert!(res.is_error);
    assert!(
        res.output.contains("Max invocation depth"),
        "Expected max depth error, got: {}",
        res.output
    );
}

#[tokio::test]
async fn test_interceptor_pipeline_pre_and_post_execution() {
    let pre_count = Arc::new(AtomicUsize::new(0));
    let post_count = Arc::new(AtomicUsize::new(0));

    let interceptor = Arc::new(TestAuditInterceptor {
        pre_count: Arc::clone(&pre_count),
        post_count: Arc::clone(&post_count),
        deny_tool: None,
    });

    let executor = executor_with(
        vec![Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>],
        vec![interceptor as Arc<dyn InterceptorPlugin>],
    );
    let call = ToolCall::new_function("call_5", "echo", json!({ "msg": "audit test" }).to_string());
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(!res.is_error);
    assert_eq!(pre_count.load(Ordering::SeqCst), 1);
    assert_eq!(post_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_interceptor_pipeline_denial() {
    let interceptor = Arc::new(TestAuditInterceptor {
        pre_count: Arc::new(AtomicUsize::new(0)),
        post_count: Arc::new(AtomicUsize::new(0)),
        deny_tool: Some("echo".to_string()),
    });

    let executor = executor_with(
        vec![Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>],
        vec![interceptor as Arc<dyn InterceptorPlugin>],
    );
    let call = ToolCall::new_function(
        "call_6",
        "echo",
        json!({ "msg": "should be blocked" }).to_string(),
    );
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(res.is_error);
    assert!(res
        .output
        .contains("Forbidden tool 'echo' blocked by interceptor"));
}

#[tokio::test]
async fn test_trace_interceptor_records_telemetry() {
    use std::sync::Mutex;
    use thunder_agent_policy::trace::{ToolTrace, ToolTraceSink, TraceInterceptor};

    struct TestSink {
        records: Mutex<Vec<ToolTrace>>,
    }
    impl ToolTraceSink for TestSink {
        fn record(&self, trace: &ToolTrace) {
            self.records.lock().unwrap().push(trace.clone());
        }
    }

    let sink = Arc::new(TestSink {
        records: Mutex::new(Vec::new()),
    });
    let interceptor = Arc::new(TraceInterceptor::new(sink.clone()));

    let executor = executor_with(
        vec![Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>],
        vec![interceptor as Arc<dyn InterceptorPlugin>],
    );
    let call = ToolCall::new_function("call_7", "echo", json!({ "msg": "trace test" }).to_string());
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(!res.is_error);
    let traces = sink.records.lock().unwrap();
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].tool, "echo");
    assert!(traces[0].result.output_preview.contains("Echo: trace test"));
}

#[tokio::test]
async fn test_interceptor_priority_ordering() {
    use std::sync::Mutex;

    struct PriorityInterceptor {
        id: &'static str,
        priority: i32,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Plugin for PriorityInterceptor {
        fn id(&self) -> &str {
            self.id
        }
    }

    #[async_trait]
    impl InterceptorPlugin for PriorityInterceptor {
        fn priority(&self) -> i32 {
            self.priority
        }

        async fn pre_execute(
            &self,
            _tool: &str,
            _args: &serde_json::Value,
            _ctx: &ToolExecutionContext,
        ) -> Result<PreExecutionVerdict, String> {
            self.log.lock().unwrap().push(format!("{}:pre", self.id));
            Ok(PreExecutionVerdict::Allow)
        }

        async fn post_execute(
            &self,
            _tool: &str,
            _args: &serde_json::Value,
            _result: &mut CallToolResult,
            _ctx: &ToolExecutionContext,
        ) {
            self.log.lock().unwrap().push(format!("{}:post", self.id));
        }
    }

    let log = Arc::new(Mutex::new(Vec::new()));

    // Intentionally register low priority (100) first, and high priority (1000) second
    let low = Arc::new(PriorityInterceptor {
        id: "low",
        priority: 100,
        log: log.clone(),
    });
    let high = Arc::new(PriorityInterceptor {
        id: "high",
        priority: 1000,
        log: log.clone(),
    });

    let executor = executor_with(
        vec![Arc::new(EchoPlugin) as Arc<dyn ToolPlugin>],
        vec![
            low as Arc<dyn InterceptorPlugin>,
            high as Arc<dyn InterceptorPlugin>,
        ],
    );
    let call = ToolCall::new_function("call_prio", "echo", json!({ "msg": "prio" }).to_string());
    let res = executor
        .run_tool(&call, ToolExecutionContext::default(), None)
        .await;

    assert!(!res.is_error);
    let sequence = log.lock().unwrap().clone();
    // Pre-execute runs high then low; Post-execute unwinds low then high (Onion Model)
    assert_eq!(
        sequence,
        vec!["high:pre", "low:pre", "low:post", "high:post"]
    );
}
