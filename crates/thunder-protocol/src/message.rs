use crate::schema::ToolDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TurnStats {
    pub turn: usize,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cached_tokens: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_tokens: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reasoning_tokens: usize,
    pub total_tokens: usize,
    pub tool_calls_count: usize,
    pub duration_ms: u64,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: ToolCallFunction,
}

impl ToolCall {
    pub fn new_function(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        #[serde(rename = "mimeType")]
        mime_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
    },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn image(mime_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            mime_type: mime_type.into(),
            data: data.into(),
            name: None,
            path: None,
            sha256: None,
        }
    }

    pub fn is_image(&self) -> bool {
        matches!(self, Self::Image { .. })
    }

    pub fn as_image(&self) -> Option<(&str, &str)> {
        match self {
            Self::Image {
                mime_type, data, ..
            } => Some((mime_type.as_str(), data.as_str())),
            Self::Text { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<Vec<ContentPart>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stats: Option<TurnStats>,
    },
    Tool {
        tool_call_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
            parts: None,
            name: None,
        }
    }

    pub fn user_multimodal(text: impl Into<String>, parts: Vec<ContentPart>) -> Self {
        Self::User {
            content: text.into(),
            parts: if parts.is_empty() { None } else { Some(parts) },
            name: None,
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Option<Vec<ToolCall>>) -> Self {
        Self::Assistant {
            content,
            tool_calls,
            refusal: None,
            name: None,
            reasoning: None,
            stats: None,
        }
    }

    pub fn assistant_turn(
        content: Option<String>,
        tool_calls: Option<Vec<ToolCall>>,
        reasoning: Option<String>,
        stats: Option<TurnStats>,
    ) -> Self {
        Self::Assistant {
            content,
            tool_calls,
            refusal: None,
            name: None,
            reasoning: reasoning.filter(|r| !r.is_empty()),
            stats,
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            refusal: None,
            name: None,
            reasoning: None,
            stats: None,
        }
    }

    pub fn reasoning_str(&self) -> Option<&str> {
        match self {
            Self::Assistant { reasoning, .. } => reasoning.as_deref(),
            _ => None,
        }
    }

    pub fn turn_stats(&self) -> Option<&TurnStats> {
        match self {
            Self::Assistant { stats, .. } => stats.as_ref(),
            _ => None,
        }
    }

    pub fn tool(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        name: Option<String>,
    ) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            name,
        }
    }

    pub fn role(&self) -> Role {
        match self {
            Self::System { .. } => Role::System,
            Self::User { .. } => Role::User,
            Self::Assistant { .. } => Role::Assistant,
            Self::Tool { .. } => Role::Tool,
        }
    }

    pub fn content_str(&self) -> Option<&str> {
        match self {
            Self::System { content, .. } => Some(content.as_str()),
            Self::User { content, .. } => Some(content.as_str()),
            Self::Assistant { content, .. } => content.as_deref(),
            Self::Tool { content, .. } => Some(content.as_str()),
        }
    }

    pub fn parts(&self) -> &[ContentPart] {
        match self {
            Self::User {
                parts: Some(parts), ..
            } => parts.as_slice(),
            _ => &[],
        }
    }

    pub fn image_parts(&self) -> impl Iterator<Item = &ContentPart> {
        self.parts().iter().filter(|p| p.is_image())
    }

    pub fn has_images(&self) -> bool {
        self.parts().iter().any(|p| p.is_image())
    }

    pub fn image_count(&self) -> usize {
        self.parts().iter().filter(|p| p.is_image()).count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    pub fn new_function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
                strict: None,
            },
        }
    }
}

impl From<&ToolDescriptor> for ToolDefinition {
    fn from(desc: &ToolDescriptor) -> Self {
        Self::new_function(
            desc.name.clone(),
            desc.description.clone(),
            desc.input_schema.clone(),
        )
    }
}

impl From<ToolDescriptor> for ToolDefinition {
    fn from(desc: ToolDescriptor) -> Self {
        Self::new_function(desc.name, desc.description, desc.input_schema)
    }
}

/// Prompt-cache pricing (per million tokens) and TTL for one model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCacheWarmSettings {
    pub ttl_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_price_per_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_price_per_m: Option<f64>,
    #[serde(
        default = "replay_safe_default",
        skip_serializing_if = "is_replay_safe"
    )]
    pub replay_safe: bool,
}

fn replay_safe_default() -> bool {
    true
}

fn is_replay_safe(v: &bool) -> bool {
    *v
}
