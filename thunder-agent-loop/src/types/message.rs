use serde::{Deserialize, Serialize};

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

/// One piece of a multimodal message.
///
/// This is Thunder's native wire shape; the pi-bridge sidecar maps it onto
/// pi-ai's content blocks (`{type:"text"}` / `{type:"image", mimeType, data}`).
///
/// `ChatMessage::User` already carries the plain-text projection in its
/// `content` field, so most user messages only add image parts here. The
/// `Text` variant exists so a future caller can interleave text and images
/// without another schema change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        /// e.g. `image/png` (magic-byte validated at ingress).
        #[serde(rename = "mimeType")]
        mime_type: String,
        /// Base64 payload, without a `data:` prefix.
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// Persistence reference: set when the bytes were offloaded to disk by
        /// the conversation store and `data` was cleared.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Content hash of the offloaded bytes (dedup key).
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

    /// `(mime_type, data)` for an image part.
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
        /// Optional multimodal content (image attachments, or interleaved
        /// text/image parts). Absent for plain-text messages, which keeps old
        /// transcripts and the text-only wire shape byte-for-byte unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parts: Option<Vec<ContentPart>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        // Must ALWAYS serialize content (null if None) to satisfy OpenAI/DeepSeek API standards
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
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

    /// A user message carrying multimodal parts alongside its text projection.
    ///
    /// `text` remains the trigger/title/`content_str()` source; `parts` carries
    /// the attachments (images). An empty `parts` collapses to a plain
    /// [`ChatMessage::user`] so callers never emit an empty `parts` array.
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
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: None,
            refusal: None,
            name: None,
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

    /// All multimodal parts of a message (empty for text-only messages).
    pub fn parts(&self) -> &[ContentPart] {
        match self {
            Self::User {
                parts: Some(parts), ..
            } => parts.as_slice(),
            _ => &[],
        }
    }

    /// Iterator over the image parts only.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_user_message_omits_parts_field() {
        // Backward compatibility: text-only messages must serialize exactly as
        // before — no `parts` key — so old transcripts and wire consumers are
        // unaffected.
        let msg = ChatMessage::user("hello");
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hello");
        assert!(v.get("parts").is_none());

        let back: ChatMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn multimodal_user_message_round_trips() {
        let msg = ChatMessage::user_multimodal(
            "describe",
            vec![
                ContentPart::text("look"),
                ContentPart::image("image/png", "aGVsbG8="),
            ],
        );
        let v = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["parts"][0]["type"], "text");
        assert_eq!(v["parts"][1]["type"], "image");
        assert_eq!(v["parts"][1]["mimeType"], "image/png");
        assert_eq!(v["parts"][1]["data"], "aGVsbG8=");
        // Optional persistence fields stay off the wire when unset.
        assert!(v["parts"][1].get("path").is_none());
        assert!(v["parts"][1].get("sha256").is_none());

        let back: ChatMessage = serde_json::from_value(v).unwrap();
        assert_eq!(back, msg);
        assert_eq!(back.image_count(), 1);
        assert!(back.has_images());
    }

    #[test]
    fn empty_parts_collapse_to_plain_message() {
        let msg = ChatMessage::user_multimodal("hi", Vec::new());
        assert_eq!(msg, ChatMessage::user("hi"));
        assert!(!msg.has_images());
        assert_eq!(msg.parts().len(), 0);
    }
}
