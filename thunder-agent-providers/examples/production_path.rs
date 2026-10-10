//! Exercise the **production seam** end to end: registry → `client_for` → real
//! `AgentLoop` → real provider, with the tool round trip included.
//!
//! ```text
//! cargo run -p thunder-agent-providers --example production_path [selection]
//! ```
//!
//! With no argument it picks the first available model the registry resolves.
//! A second pass feeds the same seam a dialect this build cannot stream, to show
//! that it fails loudly and names the reason rather than streaming nothing.
//!
//! `warn`-level tracing is installed and teed into a buffer, so "the client was
//! built from the descriptor" and "the descriptor lost a field" are observed,
//! not assumed.

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use thunder_agent_loop::prelude::{
    AgentConfig, AgentLoop, AgentTool, FunctionDefinition, LLMClientTrait, ToolDefinition,
    ToolExecutionContext,
};
use thunder_agent_providers::catalog::{ModelSpec, ProviderRegistry};
use thunder_agent_providers::client_for;

const REQUEST_TIMEOUT_MS: u64 = 180_000;

struct WeatherTool;

#[async_trait]
impl AgentTool for WeatherTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: FunctionDefinition {
                name: "get_weather".into(),
                description: "Get the current weather for a city.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
                strict: None,
            },
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolExecutionContext) -> Result<String, String> {
        let city = args
            .get("city")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        Ok(format!("{city}: 22 degrees and sunny."))
    }
}

/// A `MakeWriter` that keeps every formatted line while still printing it, so
/// diagnosing a run stays as easy as reading stderr.
#[derive(Clone, Default)]
struct Tee(Arc<Mutex<Vec<String>>>);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        self.0
            .lock()
            .expect("tee buffer")
            .push(String::from_utf8_lossy(buf).into_owned());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tee {
    type Writer = Tee;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_target(false)
        .with_writer(Tee::default())
        .init();

    let registry = ProviderRegistry::load_default().await?;
    let wanted = std::env::args().nth(1);
    let spec = pick_spec(&registry, wanted.as_deref())?;
    println!(
        "selected={} provider={} api={} available={}",
        spec.selection_id(),
        spec.provider,
        spec.api.as_pi_api_str(),
        spec.available
    );

    println!("\n=== real turn through client_for ===");
    let client = client_for(&spec, REQUEST_TIMEOUT_MS)?;
    run_turn(client, &spec).await;

    // A dialect this build cannot stream. No model in the local registry uses
    // one, so the spec is a synthetic copy of the picked model — only the
    // rejection path is under test, and it needs no endpoint.
    let mut unsupported = spec.clone();
    unsupported.api = thunder_agent_providers::api::ProviderApi::GoogleGenerateContent;
    println!(
        "\n=== unsupported dialect: api={} ===",
        unsupported.api.as_pi_api_str()
    );
    match client_for(&unsupported, REQUEST_TIMEOUT_MS) {
        Ok(_) => println!("  client built (unexpected: this dialect has no provider)"),
        Err(err) => println!("  rejected: {err}"),
    }

    Ok(())
}

fn pick_spec(registry: &ProviderRegistry, wanted: Option<&str>) -> Result<ModelSpec, String> {
    if let Some(selection) = wanted {
        return registry
            .resolve(selection)
            .cloned()
            .ok_or_else(|| format!("no model `{selection}` in the registry"));
    }
    registry
        .list_available()
        .into_iter()
        .find(|spec| spec.available)
        .cloned()
        .ok_or_else(|| "no available model in the registry".to_string())
}

async fn run_turn(client: Arc<dyn LLMClientTrait>, spec: &ModelSpec) {
    let config = AgentConfig::new(&spec.id)
        .with_system_prompt("Use the provided tool when it is called for. Be terse.");
    let mut agent = AgentLoop::new(config).with_custom_client(client);
    agent.register_tool(Arc::new(WeatherTool));

    let started = std::time::Instant::now();
    match agent
        .run("What is the weather in Shanghai? Use the tool.", None)
        .await
    {
        Ok(result) => println!(
            "{}",
            json!({
                "ms": started.elapsed().as_millis(),
                "finish_reason": format!("{:?}", result.finish_reason),
                "final_content": result.final_content,
                "turns": result.stats.total_turns,
                "prompt_tokens": result.stats.total_prompt_tokens,
                "cached_tokens": result.stats.total_cached_tokens,
            })
        ),
        Err(err) => println!("  run failed: {err:?}"),
    }
}
