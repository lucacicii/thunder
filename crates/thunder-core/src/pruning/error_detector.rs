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

pub fn extract_context_overflow_limit(err: &str) -> Option<usize> {
    if !is_context_overflow_error(err) {
        return None;
    }

    for re in [&*RE_OPENAI, &*RE_ANTHROPIC, &*RE_RANGE, &*RE_GENERIC_LIMIT] {
        if let Some(caps) = re.captures(err) {
            if let Some(m) = caps.get(1) {
                if let Ok(val) = m.as_str().parse::<usize>() {
                    if val >= 1000 {
                        return Some(val);
                    }
                }
            }
        }
    }

    None
}

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
    fn extracts_openai_and_anthropic_overflow_limits() {
        let err = "This model's maximum context length is 128000 tokens. However, your messages resulted in 135000 tokens.";
        assert_eq!(extract_context_overflow_limit(err), Some(128000));

        let anthropic = "prompt is too long: 215000 tokens > 200000 maximum";
        assert_eq!(extract_context_overflow_limit(anthropic), Some(200000));
    }
}
