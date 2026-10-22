use thunder_protocol::{ChatMessage, ChatRequestOptions, LLMClientTrait, LLMStreamChunk};
use tokio_util::sync::CancellationToken;

const SUGGESTION_SYSTEM_PROMPT: &str = "\
You are an expert developer assistant. Given the last turn of conversation, \
suggest 1 to 3 short, concrete, actionable follow-up questions or commands \
the user is most likely to ask next.

Output ONLY the questions, one per line. Do NOT include numbers, bullet points, \
or conversational filler. Keep each line under 35 characters.";

pub async fn generate_llm_suggestions(
    client: &dyn LLMClientTrait,
    model: &str,
    last_user_prompt: &str,
    last_assistant_reply: &str,
) -> Result<Vec<String>, String> {
    let prompt = format!(
        "User asked: {}\nAssistant replied: {}\n\nSuggest 1 to 3 short follow-up questions:",
        last_user_prompt, last_assistant_reply
    );

    let options = ChatRequestOptions {
        messages: vec![
            ChatMessage::system(SUGGESTION_SYSTEM_PROMPT),
            ChatMessage::user(prompt),
        ],
        tools: Vec::new(),
        model: Some(model.to_string()),
        temperature: Some(0.3),
        top_p: None,
        max_tokens: Some(80),
        thinking_level: Some("off".to_string()),
        cache_retention: Some("none".to_string()),
        session_id: None,
    };

    let cancel = CancellationToken::new();
    let mut rx = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        client.stream_chat(options, cancel),
    )
    .await
    .map_err(|_| "Timed out generating LLM suggestions".to_string())??;

    let mut content = String::new();
    while let Some(chunk) = rx.recv().await {
        match chunk? {
            LLMStreamChunk::Token(delta) => content.push_str(&delta),
            LLMStreamChunk::Completed {
                content: Some(c), ..
            } if !c.is_empty() => {
                content = c;
            }
            _ => {}
        }
    }

    let items: Vec<String> = content
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(|c: char| {
                    c.is_ascii_digit() || c == '.' || c == '-' || c == '*' || c == ' '
                })
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty() && l.chars().count() <= 50)
        .take(3)
        .collect();

    if items.is_empty() {
        return Err("No valid suggestions parsed".to_string());
    }

    Ok(items)
}
