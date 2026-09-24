use crate::api::ProviderApi;
use crate::catalog::ModelSpec;
use crate::config::sort_thinking_levels;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingProbeResult {
    pub thinking_levels: Vec<String>,
    pub default_thinking_level: String,
}

/// Parse error responses from various LLM gateway implementations
/// to extract allowed/supported reasoning_effort or thinking enum options.
pub fn extract_allowed_levels_from_error(error_text: &str) -> Option<Vec<String>> {
    let lower = error_text.to_lowercase();

    // Check if the model explicitly does not support reasoning
    if lower.contains("reasoning_effort is not supported")
        || lower.contains("reasoning_effort not supported")
        || lower.contains("unrecognized request argument supplied: reasoning_effort")
        || lower.contains("unknown parameter: reasoning_effort")
        || lower.contains("unexpected keyword argument 'reasoning_effort'")
        || lower.contains("thinking is not supported")
        || lower.contains("thinking is not enabled")
    {
        return Some(vec!["off".to_string()]);
    }

    // Known keywords we scan for in the error payload
    let candidate_tokens = ["minimal", "low", "medium", "high", "xhigh", "max"];

    // Check if error contains enum/list markers
    let has_enum_hint = lower.contains("supported values")
        || lower.contains("must be one of")
        || lower.contains("input should be")
        || lower.contains("valid choices")
        || lower.contains("expected one of")
        || lower.contains("allowed values")
        || lower.contains("valid values")
        || lower.contains("value_error");

    if has_enum_hint {
        let mut found = Vec::new();
        // Also look for "off" or "none" in the error text
        if lower.contains("'off'") || lower.contains("\"off\"") || lower.contains(" off ") || lower.contains("[off") {
            found.push("off".to_string());
        }
        if lower.contains("'none'") || lower.contains("\"none\"") || lower.contains(" none ") || lower.contains("[none") {
            found.push("none".to_string());
        }

        for token in candidate_tokens {
            // Match with quotes or boundary delimiters to avoid substring false positives
            if lower.contains(&format!("'{token}'"))
                || lower.contains(&format!("\"{token}\""))
                || lower.contains(&format!(" {token}"))
                || lower.contains(&format!("[{token}"))
                || lower.contains(&format!(",{token}"))
                || lower.contains(&format!(", {token}"))
            {
                found.push(token.to_string());
            }
        }

        if !found.is_empty() {
            if !found.contains(&"off".to_string()) && !found.contains(&"none".to_string()) {
                found.insert(0, "off".to_string());
            }
            return Some(sort_thinking_levels(&found));
        }
    }

    None
}

/// Actively probe a model's thinking/reasoning capability via minimal test HTTP calls.
pub async fn probe_model_thinking_levels(
    client: &reqwest::Client,
    spec: &ModelSpec,
) -> Option<ThinkingProbeResult> {
    if !spec.available {
        return None;
    }

    match spec.api {
        ProviderApi::OpenAiCompletions | ProviderApi::OpenAiResponses => {
            probe_openai_reasoning(client, spec).await
        }
        ProviderApi::AnthropicMessages => probe_anthropic_thinking(client, spec).await,
        ProviderApi::GoogleGenerateContent => probe_google_thinking(client, spec).await,
        ProviderApi::Ollama => None,
    }
}

async fn probe_openai_reasoning(
    client: &reqwest::Client,
    spec: &ModelSpec,
) -> Option<ThinkingProbeResult> {
    let url = if spec.api == ProviderApi::OpenAiResponses {
        if spec.base_url.ends_with("/responses") {
            spec.base_url.clone()
        } else {
            format!("{}/responses", spec.base_url.trim_end_matches('/'))
        }
    } else {
        if spec.base_url.ends_with("/chat/completions") {
            spec.base_url.clone()
        } else {
            format!("{}/chat/completions", spec.base_url.trim_end_matches('/'))
        }
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(ref key) = spec.api_key {
        if !key.is_empty() {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {key}")) {
                headers.insert(AUTHORIZATION, val);
            }
        }
    }
    for (k, v) in &spec.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            headers.insert(name, val);
        }
    }

    // Step 1: Send intentional probe effort "__probe__" to trigger schema validation error with allowed enums
    let probe_payload = if spec.api == ProviderApi::OpenAiResponses {
        json!({
            "model": spec.id,
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "1"}]}],
            "max_output_tokens": 1,
            "reasoning_effort": "__probe__"
        })
    } else {
        json!({
            "model": spec.id,
            "messages": [{"role": "user", "content": "1"}],
            "max_tokens": 1,
            "reasoning_effort": "__probe__"
        })
    };

    let resp = client
        .post(&url)
        .headers(headers.clone())
        .timeout(Duration::from_secs(5))
        .json(&probe_payload)
        .send()
        .await;

    match resp {
        Ok(res) => {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            debug!("Probe response for {}: status={}, body={}", spec.id, status, body);

            // If 400 Bad Request, inspect body for allowed levels or non-support
            if status.is_client_error() {
                if let Some(levels) = extract_allowed_levels_from_error(&body) {
                    let default_level = if levels.contains(&"medium".to_string()) {
                        "medium".to_string()
                    } else if levels.contains(&"high".to_string()) {
                        "high".to_string()
                    } else {
                        levels.first().cloned().unwrap_or_else(|| "off".to_string())
                    };
                    return Some(ThinkingProbeResult {
                        thinking_levels: levels,
                        default_thinking_level: default_level,
                    });
                }
            }

            // Step 2: If the gateway returned 200 or generic error, test "low"
            let test_payload = if spec.api == ProviderApi::OpenAiResponses {
                json!({
                    "model": spec.id,
                    "input": [{"role": "user", "content": [{"type": "input_text", "text": "1"}]}],
                    "max_output_tokens": 1,
                    "reasoning_effort": "low"
                })
            } else {
                json!({
                    "model": spec.id,
                    "messages": [{"role": "user", "content": "1"}],
                    "max_tokens": 1,
                    "reasoning_effort": "low"
                })
            };

            let test_resp = client
                .post(&url)
                .headers(headers)
                .timeout(Duration::from_secs(5))
                .json(&test_payload)
                .send()
                .await;

            if let Ok(tres) = test_resp {
                if tres.status().is_success() {
                    return Some(ThinkingProbeResult {
                        thinking_levels: vec![
                            "off".to_string(),
                            "low".to_string(),
                            "medium".to_string(),
                            "high".to_string(),
                        ],
                        default_thinking_level: "medium".to_string(),
                    });
                }
            }

            None
        }
        Err(e) => {
            warn!("Active thinking probe network error for {}: {e}", spec.id);
            None
        }
    }
}

async fn probe_anthropic_thinking(
    client: &reqwest::Client,
    spec: &ModelSpec,
) -> Option<ThinkingProbeResult> {
    let url = if spec.base_url.ends_with("/messages") {
        spec.base_url.clone()
    } else {
        format!("{}/messages", spec.base_url.trim_end_matches('/'))
    };

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        HeaderName::from_static("anthropic-version"),
        HeaderValue::from_static("2023-06-01"),
    );
    if let Some(ref key) = spec.api_key {
        if !key.is_empty() {
            if let Ok(val) = HeaderValue::from_str(key) {
                headers.insert(HeaderName::from_static("x-api-key"), val);
            }
        }
    }
    for (k, v) in &spec.headers {
        if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
            headers.insert(name, val);
        }
    }

    let payload = json!({
        "model": spec.id,
        "max_tokens": 1025,
        "messages": [{"role": "user", "content": "1"}],
        "thinking": {
            "type": "enabled",
            "budget_tokens": 1024
        }
    });

    let resp = client
        .post(&url)
        .headers(headers)
        .timeout(Duration::from_secs(5))
        .json(&payload)
        .send()
        .await;

    if let Ok(res) = resp {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        if status.is_success() || body.contains("budget_tokens") {
            return Some(ThinkingProbeResult {
                thinking_levels: vec![
                    "off".to_string(),
                    "low".to_string(),
                    "medium".to_string(),
                    "high".to_string(),
                ],
                default_thinking_level: "medium".to_string(),
            });
        }
        if body.contains("thinking is not supported") || body.contains("unrecognized field thinking") {
            return Some(ThinkingProbeResult {
                thinking_levels: vec!["off".to_string()],
                default_thinking_level: "off".to_string(),
            });
        }
    }

    None
}

async fn probe_google_thinking(
    _client: &reqwest::Client,
    spec: &ModelSpec,
) -> Option<ThinkingProbeResult> {
    if spec.id.to_lowercase().contains("thinking") {
        Some(ThinkingProbeResult {
            thinking_levels: vec![
                "off".to_string(),
                "low".to_string(),
                "medium".to_string(),
                "high".to_string(),
            ],
            default_thinking_level: "medium".to_string(),
        })
    } else {
        Some(ThinkingProbeResult {
            thinking_levels: vec!["off".to_string()],
            default_thinking_level: "off".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_allowed_levels_from_openai_errors() {
        let err1 = "Invalid value for 'reasoning_effort': '__probe__'. Supported values are: 'low', 'medium', 'high'.";
        let res1 = extract_allowed_levels_from_error(err1).unwrap();
        assert_eq!(res1, vec!["off", "low", "medium", "high"]);

        let err2 = "1 validation error for Request\nbody -> reasoning_effort\n  Input should be 'low', 'medium' or 'high'";
        let res2 = extract_allowed_levels_from_error(err2).unwrap();
        assert_eq!(res2, vec!["off", "low", "medium", "high"]);

        let err3 = "reasoning_effort must be one of: ['low', 'high', 'max']";
        let res3 = extract_allowed_levels_from_error(err3).unwrap();
        assert_eq!(res3, vec!["off", "low", "high", "max"]);

        let err_unsupported = "Unrecognized request argument supplied: reasoning_effort";
        let res_unsup = extract_allowed_levels_from_error(err_unsupported).unwrap();
        assert_eq!(res_unsup, vec!["off"]);
    }
}
