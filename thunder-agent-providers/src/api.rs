use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApi {
    #[default]
    #[serde(alias = "openai", alias = "openai-chat", alias = "chat-completions")]
    OpenAiCompletions,
    #[serde(alias = "openai-response", alias = "responses")]
    OpenAiResponses,
    #[serde(alias = "anthropic", alias = "claude", alias = "anthropic-messages", alias = "anthropic-chat")]
    AnthropicMessages,
    #[serde(alias = "google", alias = "gemini", alias = "google-generate-content", alias = "google-gemini")]
    GoogleGenerateContent,
    #[serde(alias = "ollama", alias = "ollama-chat")]
    Ollama,
}

impl ProviderApi {
    pub fn from_str_loose(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "openai-completions" | "openai" | "openai-chat" | "chat-completions" => {
                Some(Self::OpenAiCompletions)
            }
            "openai-responses" | "openai-response" | "responses" => Some(Self::OpenAiResponses),
            "anthropic-messages" | "anthropic" | "claude" | "anthropic-chat" => {
                Some(Self::AnthropicMessages)
            }
            "google-generate-content" | "google" | "gemini" | "google-gemini" => {
                Some(Self::GoogleGenerateContent)
            }
            "ollama" | "ollama-chat" => Some(Self::Ollama),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAiCompletions => "openai-completions",
            Self::OpenAiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
            Self::GoogleGenerateContent => "google-generate-content",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }

    pub fn parse(raw: &str) -> Self {
        if let Some((provider, model)) = raw.split_once('/') {
            if !provider.is_empty() && !model.is_empty() {
                return Self::new(provider, model);
            }
        }
        Self::new("openai", raw)
    }

    pub fn selection_id(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }
}
