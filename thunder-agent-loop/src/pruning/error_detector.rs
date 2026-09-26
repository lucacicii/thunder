use regex::Regex;
use std::sync::LazyLock;

static RE_OPENAI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)maximum context length is (\d+) tokens?").unwrap());

static RE_ANTHROPIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)>\s*(\d+)\s*maximum").unwrap());

static RE_RANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)should be \[\s*\d+\s*,\s*(\d+)\s*\]").unwrap());

static RE_GENERIC_LIMIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:maximum limit of|maximum context window of|maximum allowed (?:tokens )?is|limit of)\s*(\d+)").unwrap()
});

/// Extracts the authentic maximum context token limit from official LLM provider error messages.
/// Returns Some(max_tokens) if an overflow error was identified, or None if unrelated.
pub fn extract_context_overflow_limit(err: &str) -> Option<usize> {
    let lower = err.to_lowercase();
    let is_overflow_indicator = lower.contains("context length")
        || lower.contains("context_length_exceeded")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("maximum context")
        || lower.contains("exceeds the maximum")
        || lower.contains("range of input length")
        || lower.contains("too many tokens")
        || lower.contains("token limit exceeded");

    if !is_overflow_indicator {
        return None;
    }

    // 1. Check OpenAI / DeepSeek format
    if let Some(caps) = RE_OPENAI.captures(err) {
        if let Some(m) = caps.get(1) {
            if let Ok(val) = m.as_str().parse::<usize>() {
                if val >= 1000 {
                    return Some(val);
                }
            }
        }
    }

    // 2. Check Anthropic format (> 200000 maximum)
    if let Some(caps) = RE_ANTHROPIC.captures(err) {
        if let Some(m) = caps.get(1) {
            if let Ok(val) = m.as_str().parse::<usize>() {
                if val >= 1000 {
                    return Some(val);
                }
            }
        }
    }

    // 3. Check Range format ([1, 131072])
    if let Some(caps) = RE_RANGE.captures(err) {
        if let Some(m) = caps.get(1) {
            if let Ok(val) = m.as_str().parse::<usize>() {
                if val >= 1000 {
                    return Some(val);
                }
            }
        }
    }

    // 4. Generic limits
    if let Some(caps) = RE_GENERIC_LIMIT.captures(err) {
        if let Some(m) = caps.get(1) {
            if let Ok(val) = m.as_str().parse::<usize>() {
                if val >= 1000 {
                    return Some(val);
                }
            }
        }
    }

    // If an overflow keyword was present but no specific digits matched,
    // return a sentinel default indicator (e.g. None or caller can halve threshold)
    None
}

/// Helper to check whether an error string indicates a context overflow condition.
pub fn is_context_overflow_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("context length")
        || lower.contains("context_length_exceeded")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("maximum context")
        || lower.contains("exceeds the maximum")
        || lower.contains("range of input length")
        || lower.contains("too many tokens")
        || lower.contains("token limit exceeded")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_openai_overflow() {
        let err = "This model's maximum context length is 128000 tokens. However, your messages resulted in 135000 tokens.";
        assert_eq!(extract_context_overflow_limit(err), Some(128000));

        let deepseek = "maximum context length is 65536 tokens";
        assert_eq!(extract_context_overflow_limit(deepseek), Some(65536));
    }

    #[test]
    fn test_extract_anthropic_overflow() {
        let err = "prompt is too long: 215000 tokens > 200000 maximum";
        assert_eq!(extract_context_overflow_limit(err), Some(200000));
    }

    #[test]
    fn test_extract_qwen_range_overflow() {
        let err = "Range of input length should be [1, 131072]";
        assert_eq!(extract_context_overflow_limit(err), Some(131072));
    }

    #[test]
    fn test_unrelated_errors() {
        let rate_limit = "Rate limit exceeded (429): Too many requests.";
        assert_eq!(extract_context_overflow_limit(rate_limit), None);
        assert!(!is_context_overflow_error(rate_limit));

        let auth_err = "Unauthorized: Invalid API Key.";
        assert_eq!(extract_context_overflow_limit(auth_err), None);
        assert!(!is_context_overflow_error(auth_err));
    }
}
