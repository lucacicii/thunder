use crate::types::message::ToolCall;
use bytes::Bytes;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct StreamDelta {
    pub content_delta: Option<String>,
    /// Reasoning / chain-of-thought tokens (DeepSeek-R1, o1 series). Kept separate
    /// from `content_delta` so thinking output never pollutes the final answer.
    pub reasoning_delta: Option<String>,
    pub tool_calls_delta: Option<Vec<StreamToolCallDelta>>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<usize>,
    pub completion_tokens: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct StreamToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: Option<String>,
}

#[derive(Debug, Default)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    choices: Option<Vec<OpenAIChoice>>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    delta: Option<OpenAIDelta>,
    message: Option<OpenAIDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    reasoning: Option<String>,
    text: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
}

pub struct SSEStreamParser {
    buffer: String,
    accumulated_tools: BTreeMap<usize, AccumulatedToolCall>,
}

impl Default for SSEStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SSEStreamParser {
    pub fn new() -> Self {
        Self {
            buffer: String::with_capacity(4096),
            accumulated_tools: BTreeMap::new(),
        }
    }

    /// Feed incoming raw network chunk and parse out SSE events
    pub fn feed_chunk(&mut self, chunk: &Bytes) -> Vec<StreamDelta> {
        let chunk_str = String::from_utf8_lossy(chunk);
        self.buffer.push_str(&chunk_str);

        let mut results = Vec::new();

        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim().to_string();
            self.buffer.drain(..=pos);

            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }

                if let Some(delta) = self.parse_data(data) {
                    results.push(delta);
                }
            }
        }

        results
    }

    /// Flush any remaining buffered data
    pub fn flush(&mut self) -> Option<StreamDelta> {
        let trimmed = self.buffer.trim().to_string();
        self.buffer.clear();

        if let Some(data) = trimmed.strip_prefix("data:") {
            let data = data.trim();
            if data != "[DONE]" {
                return self.parse_data(data);
            }
        }
        None
    }

    /// Collect fully aggregated tool calls, guaranteeing fallback IDs and names for malformed provider chunks
    pub fn get_completed_tool_calls(&self) -> Vec<ToolCall> {
        self.accumulated_tools
            .iter()
            .map(|(index, acc)| {
                let id = if acc.id.is_empty() {
                    format!("call_{}", index)
                } else {
                    acc.id.clone()
                };
                let name = if acc.name.is_empty() {
                    format!("unnamed_tool_{}", index)
                } else {
                    acc.name.clone()
                };
                ToolCall::new_function(id, name, &acc.arguments)
            })
            .collect()
    }

    fn parse_data(&mut self, json_str: &str) -> Option<StreamDelta> {
        let parsed: OpenAIStreamChunk = serde_json::from_str(json_str).ok()?;

        let mut content_delta = None;
        let mut reasoning_delta = None;
        let mut tool_calls_delta = None;
        let mut finish_reason = None;

        if let Some(choice) = parsed.choices.and_then(|c| c.into_iter().next()) {
            let delta_opt = choice.delta.or(choice.message);
            if let Some(delta) = delta_opt {
                // Reasoning / chain-of-thought tokens are captured separately and never
                // mixed into the user-facing content stream.
                if let Some(r) = delta.reasoning_content.or(delta.reasoning) {
                    if !r.is_empty() {
                        reasoning_delta = Some(r);
                    }
                }

                if let Some(c) = delta.content.or(delta.text) {
                    if !c.is_empty() {
                        content_delta = Some(c);
                    }
                }

                if let Some(tools) = delta.tool_calls {
                    let mut tc_deltas = Vec::with_capacity(tools.len());
                    for tc in tools {
                        let acc = self.accumulated_tools.entry(tc.index).or_default();
                        if let Some(id) = &tc.id {
                            acc.id = id.clone();
                        }
                        if let Some(func) = &tc.function {
                            if let Some(name) = &func.name {
                                acc.name.push_str(name);
                            }
                            if let Some(args) = &func.arguments {
                                acc.arguments.push_str(args);
                            }
                        }

                        tc_deltas.push(StreamToolCallDelta {
                            index: tc.index,
                            id: tc.id,
                            name: tc.function.as_ref().and_then(|f| f.name.clone()),
                            arguments_delta: tc.function.and_then(|f| f.arguments),
                        });
                    }
                    tool_calls_delta = Some(tc_deltas);
                }
            }

            if let Some(fr) = choice.finish_reason {
                finish_reason = Some(fr);
            }
        }

        let mut prompt_tokens = None;
        let mut completion_tokens = None;
        if let Some(usage) = parsed.usage {
            prompt_tokens = usage.prompt_tokens;
            completion_tokens = usage.completion_tokens;
        }

        Some(StreamDelta {
            content_delta,
            reasoning_delta,
            tool_calls_delta,
            finish_reason,
            prompt_tokens,
            completion_tokens,
        })
    }
}
