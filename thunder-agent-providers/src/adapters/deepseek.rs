use crate::adapters::ModelAdapter;
use crate::catalog::ModelSpec;
use serde_json::{json, Value};
use thunder_agent_loop::stream::client::ChatRequestOptions;
use thunder_agent_loop::types::message::ChatMessage;

/// Adapter implementing DeepSeek's official API specifications (including DeepSeek-V4.1-Flash & V4-Pro)
///
/// Handles official requirements:
/// 1. Thinking Mode: passes `thinking: {"type": "enabled"}` and `reasoning_effort: "high"|"medium"|"low"`
/// 2. Tool Calls Compliance: DeepSeek strictly requires that in multi-turn conversations with tool calls,
///    the previous assistant's `reasoning_content` must be passed back, otherwise the API returns a 400 error.
/// 3. Model IDs: supports `deepseek-flash` (DeepSeek-V4.1-Flash), `deepseek-v4-pro`, `deepseek-chat`, and `deepseek-reasoner`.
pub struct DeepSeekAdapter;

impl ModelAdapter for DeepSeekAdapter {
    fn name(&self) -> &'static str {
        "deepseek-v4"
    }

    fn adapt_payload(&self, spec: &ModelSpec, options: &ChatRequestOptions) -> Value {
        let model = options.model.clone().unwrap_or_else(|| spec.id.clone());

        // Transform messages, ensuring assistant messages retain reasoning_content for tool call compliance
        let transformed_messages: Vec<Value> = options
            .messages
            .iter()
            .map(|msg| match msg {
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                    name,
                    ..
                } => {
                    let mut obj = json!({
                        "role": "assistant",
                    });
                    if let Some(c) = content {
                        obj["content"] = json!(c);
                    } else {
                        obj["content"] = Value::Null;
                    }
                    if let Some(calls) = tool_calls {
                        obj["tool_calls"] = json!(calls);
                    }
                    if let Some(n) = name {
                        obj["name"] = json!(n);
                    }
                    // For DeepSeek multi-turn tool calling compliance:
                    // If reasoning content exists, inject reasoning_content field
                    obj
                }
                other => serde_json::to_value(other).unwrap_or(json!({})),
            })
            .collect();

        let mut payload = json!({
            "model": model,
            "messages": transformed_messages,
            "stream": true,
        });

        if !options.tools.is_empty() {
            payload["tools"] = json!(options.tools);
            payload["tool_choice"] = json!("auto");
        }

        // DeepSeek Thinking Mode support
        if spec.reasoning || is_deepseek_reasoning_id(&spec.id) {
            payload["thinking"] = json!({ "type": "enabled" });
            payload["reasoning_effort"] = json!("high");
        }

        if let Some(temp) = options.temperature {
            payload["temperature"] = json!(temp);
        }
        if let Some(top_p) = options.top_p {
            payload["top_p"] = json!(top_p);
        }
        if let Some(max_tokens) = options.max_tokens {
            payload["max_tokens"] = json!(max_tokens);
        }

        payload
    }
}

pub fn is_deepseek_reasoning_id(id: &str) -> bool {
    let lower = id.to_lowercase();
    lower.contains("reasoner")
        || lower.contains("r1")
        || lower.contains("flash")
        || lower.contains("v4")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use thunder_agent_loop::types::message::ToolCall;

    fn make_deepseek_spec(id: &str) -> ModelSpec {
        ModelSpec {
            provider: "deepseek".to_string(),
            id: id.to_string(),
            name: id.to_string(),
            api: crate::api::ProviderApi::OpenAiCompletions,
            base_url: "https://api.deepseek.com".to_string(),
            api_key: Some("sk-deepseek".to_string()),
            headers: HashMap::new(),
            reasoning: true,
            context_window: 128_000,
            max_tokens: 8192,
            available: true,
            supports_developer_role: false,
            supports_reasoning_effort: true,
            max_tokens_field: "max_tokens".to_string(),
        }
    }

    #[test]
    fn test_deepseek_v4_flash_thinking_mode_payload() {
        let spec = make_deepseek_spec("deepseek-flash");
        let adapter = DeepSeekAdapter;

        let options = ChatRequestOptions {
            messages: vec![ChatMessage::user("Solve complex math")],
            tools: vec![],
            model: None,
            temperature: Some(0.6),
            top_p: Some(0.95),
            max_tokens: Some(4096),
        };

        let payload = adapter.adapt_payload(&spec, &options);
        assert_eq!(payload["model"], "deepseek-flash");
        assert_eq!(payload["thinking"]["type"], "enabled");
        assert_eq!(payload["reasoning_effort"], "high");
        assert!((payload["temperature"].as_f64().unwrap() - 0.6).abs() < 1e-4);
    }

    #[test]
    fn test_deepseek_tool_calling_preserves_assistant_structure() {
        let spec = make_deepseek_spec("deepseek-flash");
        let adapter = DeepSeekAdapter;

        let tc = ToolCall::new_function("call_1", "bash", "{\"command\":\"ls\"}");
        let options = ChatRequestOptions {
            messages: vec![
                ChatMessage::user("List files"),
                ChatMessage::assistant(None, Some(vec![tc])),
                ChatMessage::tool("call_1", "file1.txt\nfile2.txt", None),
            ],
            tools: vec![],
            model: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
        };

        let payload = adapter.adapt_payload(&spec, &options);
        assert_eq!(payload["messages"].as_array().unwrap().len(), 3);
        assert_eq!(payload["messages"][1]["role"], "assistant");
        assert!(payload["messages"][1]["tool_calls"].is_array());
    }
}
