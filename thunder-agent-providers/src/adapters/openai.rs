use crate::adapters::ModelAdapter;
use crate::catalog::ModelSpec;
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::ChatRequestOptions;
use thunder_agent_loop::types::message::ChatMessage;

/// Standard OpenAI Chat Completions adapter (GPT-4o, DeepSeek, Qwen, Moonshot, etc.)
pub struct OpenAiStandardAdapter;

impl ModelAdapter for OpenAiStandardAdapter {
    fn name(&self) -> &'static str {
        "openai-standard"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let model = options.model.clone().unwrap_or_else(|| spec.id.clone());

        // Transform system messages to developer messages if supported
        let messages: Vec<Value> = if spec.supports_developer_role {
            options
                .messages
                .iter()
                .map(|msg| match msg {
                    ChatMessage::System { content, name } => {
                        let mut obj = json!({
                            "role": "developer",
                            "content": content
                        });
                        if let Some(n) = name {
                            obj["name"] = json!(n);
                        }
                        obj
                    }
                    other => serde_json::to_value(other).unwrap_or(json!({})),
                })
                .collect()
        } else {
            options
                .messages
                .iter()
                .map(|m| serde_json::to_value(m).unwrap_or(json!({})))
                .collect()
        };

        let mut payload = json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !options.tools.is_empty() {
            payload["tools"] = json!(options.tools);
            payload["tool_choice"] = json!("auto");
        }

        if let Some(temp) = options.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(top_p) = options.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(max_tokens) = options.max_tokens {
            let field_name = &spec.max_tokens_field;
            payload[field_name] = json!(max_tokens);
        }

        // Thinking control: the DeepSeek-style `thinking` object is ONLY legal on DeepSeek-family
        // endpoints (handled by DeepSeekAdapter). Here we emit `reasoning_effort` exclusively,
        // and only when the model explicitly declares support — unknown params can 400 on strict gateways.
        if spec.supports_reasoning_effort {
            let thinking_level = options.thinking_level.as_deref().or_else(|| {
                if !spec.default_thinking_level.is_empty() && spec.default_thinking_level != "off" {
                    Some(spec.default_thinking_level.as_str())
                } else {
                    None
                }
            });

            if let Some(level) = thinking_level {
                let level_lower = level.to_lowercase();
                let effort = match level_lower.as_str() {
                    "off" | "false" | "disabled" | "low" | "minimal" => "low",
                    "medium" => "medium",
                    "high" | "max" | "xhigh" => "high",
                    other => other,
                };
                payload["reasoning_effort"] = json!(effort);
            }
        }

        payload
    }
}

/// Specialized adapter for OpenAI Reasoning models (o1, o3, o3-mini series)
/// Nuances handled:
/// - Replaces `system` role with `developer` role
/// - Strips `temperature` parameter (unsupported on o1/o3 and triggers 400 error)
/// - Uses `max_completion_tokens` instead of `max_tokens`
/// - Injects `reasoning_effort` if configured
pub struct OpenAiReasoningAdapter;

impl ModelAdapter for OpenAiReasoningAdapter {
    fn name(&self) -> &'static str {
        "openai-reasoning"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let model = options.model.clone().unwrap_or_else(|| spec.id.clone());

        // Transform system messages to developer messages
        let transformed_messages: Vec<Value> = options
            .messages
            .iter()
            .map(|msg| match msg {
                ChatMessage::System { content, name } => {
                    let mut obj = json!({
                        "role": "developer",
                        "content": content
                    });
                    if let Some(n) = name {
                        obj["name"] = json!(n);
                    }
                    obj
                }
                other => serde_json::to_value(other).unwrap_or(json!({})),
            })
            .collect();

        let mut payload = json!({
            "model": model,
            "messages": transformed_messages,
            "stream": true,
            "stream_options": { "include_usage": true }
        });

        if !options.tools.is_empty() {
            payload["tools"] = json!(options.tools);
            payload["tool_choice"] = json!("auto");
        }

        // NOTE: o1/o3 series intentionally DO NOT support temperature / top_p

        // Use max_completion_tokens for reasoning models
        if let Some(max_tokens) = options.max_tokens {
            payload["max_completion_tokens"] = json!(max_tokens);
        }

        // Add reasoning_effort if supported or needed
        if spec.supports_reasoning_effort || spec.reasoning {
            let effort = match options.thinking_level.as_deref() {
                Some("off") => "low",
                Some(e) => e,
                None => {
                    if !spec.default_thinking_level.is_empty() && spec.default_thinking_level != "off" {
                        spec.default_thinking_level.as_str()
                    } else {
                        "medium"
                    }
                }
            };
            payload["reasoning_effort"] = json!(effort);
        }

        payload
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_test_spec(id: &str) -> ModelSpec {
        ModelSpec {
            provider: "openai".to_string(),
            id: id.to_string(),
            name: id.to_string(),
            api: crate::api::ProviderApi::OpenAiCompletions,
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: Some("sk-test".to_string()),
            headers: HashMap::new(),
            reasoning: false,
            context_window: 128_000,
            max_tokens: 4096,
            available: true,
            supports_developer_role: false,
            supports_reasoning_effort: false,
            max_tokens_field: "max_tokens".to_string(),
            thinking_levels: vec!["off".to_string()],
            default_thinking_level: "off".to_string(),
            thinking_levels_probed: false,
        }
    }

    #[test]
    fn test_standard_adapter_preserves_temperature() {
        let spec = make_test_spec("gpt-4o");
        let adapter = OpenAiStandardAdapter;

        let options = ChatRequestOptions {
            messages: vec![ChatMessage::user("hello")],
            tools: vec![],
            model: None,
            temperature: Some(0.7),
            top_p: Some(0.9),
            max_tokens: Some(1024),
            thinking_level: None,
        };

        let payload = adapter.adapt_payload(&spec, &options);
        assert_eq!(payload["model"], "gpt-4o");
        assert!((payload["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-4);
        assert_eq!(payload["max_tokens"], 1024);
    }

    #[test]
    fn test_reasoning_adapter_transforms_system_and_strips_temperature() {
        let mut spec = make_test_spec("o1-preview");
        spec.supports_reasoning_effort = true;
        let adapter = OpenAiReasoningAdapter;

        let options = ChatRequestOptions {
            messages: vec![
                ChatMessage::system("system instruction"),
                ChatMessage::user("perform reasoning task"),
            ],
            tools: vec![],
            model: None,
            temperature: Some(0.7), // Should be stripped!
            top_p: Some(0.9),       // Should be stripped!
            max_tokens: Some(2048),
            thinking_level: None,
        };

        let payload = adapter.adapt_payload(&spec, &options);
        assert_eq!(payload["model"], "o1-preview");
        // Temperature and top_p must not be present
        assert!(payload.get("temperature").is_none());
        assert!(payload.get("top_p").is_none());
        // System message mapped to developer role
        assert_eq!(payload["messages"][0]["role"], "developer");
        assert_eq!(payload["messages"][0]["content"], "system instruction");
        // max_completion_tokens used instead of max_tokens
        assert_eq!(payload["max_completion_tokens"], 2048);
        assert!(payload.get("max_tokens").is_none());
        assert_eq!(payload["reasoning_effort"], "medium");
    }
}
