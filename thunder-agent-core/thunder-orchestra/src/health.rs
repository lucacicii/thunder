//! Scheduler-side health / diagnostics.
//!
//! Lets operators verify the orchestrator before launching a real pipeline:
//! writable store / scratch dirs, and a reachable (or mocked) LLM endpoint.
//! B stays a pure scheduler — `health()` never drives A's turns.

use crate::config::OrchestraConfig;
use crate::mock::RoleMockClient;
use crate::store::{RunStore, StoredRun};
use serde::Serialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thunder_agent_loop::{
    AgentRunResult, ChatMessage, ChatRequestOptions, FinishReason, LLMClientTrait,
    LLMStreamChunk,
};
use tokio::fs;
use tokio_util::sync::CancellationToken;

/// Overall verdict of a [`HealthReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    /// All critical checks passed.
    Ok,
    /// Non-critical soft failure; the orchestrator is still usable.
    Degraded,
    /// At least one critical check failed.
    Failed,
}

/// One named check and its outcome.
#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub name: String,
    #[serde(rename = "passed")]
    pub passed: bool,
    pub detail: String,
}

/// A full health report: status plus the per-check breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub status: HealthStatus,
    pub checks: Vec<HealthCheck>,
}

/// Build a transient `AgentConfig`-derived base config for diagnostics.
fn base_config(config: &OrchestraConfig) -> Option<thunder_agent_loop::AgentConfig> {
    config.base.clone().or_else(|| {
        config
            .units
            .first()
            .map(|u| u.config.clone())
    })
}

/// Probe the store dir by writing and removing an ephemeral record.
async fn check_store_writable(config: &OrchestraConfig) -> HealthCheck {
    let store = RunStore::new(config.store_root.clone());
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let run_id = format!(".health/{ts}");

    // Minimal fake result so we can reuse RunStore::save for the probe.
    let probe = AgentRunResult {
        agent_id: "health".to_string(),
        final_content: Some(String::new()),
        messages: vec![],
        stats: Default::default(),
        finish_reason: FinishReason::Done,
    };

    match store.save(&run_id, "health", &probe).await {
        Ok(path) => match fs::remove_file(&path).await {
            Ok(()) => HealthCheck {
                name: "store_writable".to_string(),
                passed: true,
                detail: format!("wrote and removed probe at {}", path.display()),
            },
            Err(e) => HealthCheck {
                name: "store_writable".to_string(),
                passed: false,
                detail: format!("wrote probe but failed to clean up: {e}"),
            },
        },
        Err(e) => HealthCheck {
            name: "store_writable".to_string(),
            passed: false,
            detail: format!("cannot write to store at {}: {e}", config.store_root.display()),
        },
    }
}

/// Probe the scratch dir by creating it and removing a probe file.
async fn check_scratch_writable(config: &OrchestraConfig) -> HealthCheck {
    let root = config.scratch_root.clone();
    let probe = root.join(".health-probe");

    match fs::create_dir_all(&root).await {
        Ok(()) => {
            match fs::write(&probe, b"ok").await {
                Ok(()) => match fs::remove_file(&probe).await {
                    Ok(()) => HealthCheck {
                        name: "scratch_writable".to_string(),
                        passed: true,
                        detail: format!("wrote and removed probe at {}", root.display()),
                    },
                    Err(e) => HealthCheck {
                        name: "scratch_writable".to_string(),
                        passed: false,
                        detail: format!("created scratch dir but failed to clean up: {e}"),
                    },
                },
                Err(e) => HealthCheck {
                    name: "scratch_writable".to_string(),
                    passed: false,
                    detail: format!("cannot write into scratch dir {}: {e}", root.display()),
                },
            }
        }
        Err(e) => HealthCheck {
            name: "scratch_writable".to_string(),
            passed: false,
            detail: format!("cannot create scratch dir {}: {e}", root.display()),
        },
    }
}

/// Issue one minimal request to confirm the (mock or live) LLM streams a
/// `Completed` chunk within the configured timeout.
async fn check_llm_reachable(config: &OrchestraConfig, use_mock: bool) -> HealthCheck {
    let Some(base) = base_config(config) else {
        return HealthCheck {
            name: "llm_reachable".to_string(),
            passed: false,
            detail: "no base AgentConfig available (set `base` or add a unit)".to_string(),
        };
    };

    let client: Arc<dyn LLMClientTrait> = if use_mock {
        Arc::new(RoleMockClient::new("health"))
    } else {
        return HealthCheck {
            name: "llm_reachable".to_string(),
            passed: false,
            detail: "live LLM probe requires a provider-injected client".to_string(),
        };
    };

    let cancel = CancellationToken::new();
    let timeout = std::time::Duration::from_millis(base.request_timeout_ms);

    let options = ChatRequestOptions {
        messages: vec![
            ChatMessage::system("health probe"),
            ChatMessage::user("ping"),
        ],
        tools: vec![],
        model: Some(base.model.clone()),
        temperature: None,
        top_p: None,
        max_tokens: Some(1),
        thinking_level: None,
    };

    let result = match tokio::time::timeout(timeout, client.stream_chat(options, cancel)).await {
        Ok(Ok(mut rx)) => {
            // Drain the stream; accept the first Completed chunk or any error.
            let mut last_err: Option<String> = None;
            loop {
                match rx.recv().await {
                    Some(Ok(LLMStreamChunk::Completed { .. })) => break Ok(()),
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        last_err = Some(e);
                        continue;
                    }
                    None => break match last_err {
                        Some(e) => Err(e),
                        None => Err("stream ended without a Completed chunk".to_string()),
                    },
                }
            }
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("LLM request timed out after {}ms", base.request_timeout_ms)),
    };

    match result {
        Ok(()) => HealthCheck {
            name: "llm_reachable".to_string(),
            passed: true,
            detail: if use_mock {
                "mock client streamed a Completed chunk".to_string()
            } else {
                "provider client streamed a Completed chunk".to_string()
            },
        },
        Err(e) => HealthCheck {
            name: "llm_reachable".to_string(),
            passed: false,
            detail: e,
        },
    }
}

/// Run the standard critical checks and assemble a [`HealthReport`].
///
/// `scheduler_smoke` is intentionally omitted by default to keep `health`
/// cheap; enable it via [`run_health`] with `smoke = true` if desired.
pub async fn run_health(config: &OrchestraConfig, use_mock: bool) -> HealthReport {
    let mut checks: Vec<HealthCheck> = Vec::new();

    checks.push(check_store_writable(config).await);
    checks.push(check_scratch_writable(config).await);
    checks.push(check_llm_reachable(config, use_mock).await);

    let failed = checks.iter().any(|c| !c.passed && is_critical(&c.name));

    let status = if failed {
        HealthStatus::Failed
    } else if checks.iter().any(|c| !c.passed) {
        HealthStatus::Degraded
    } else {
        HealthStatus::Ok
    };

    HealthReport { status, checks }
}

fn is_critical(name: &str) -> bool {
    matches!(name, "store_writable" | "scratch_writable" | "llm_reachable")
}

// Keep `StoredRun` import referenced (used as the on-disk probe record shape).
#[allow(dead_code)]
fn _assert_stored_run_marker(_: &StoredRun) {}
