/// Conservative per-image token estimate.
///
/// Vision providers charge by image area (roughly `width * height / 750`), but
/// decoding dimensions here would pull image parsing into the hot path. A flat
/// upper-bound keeps context compaction timing safe: over-estimating only
/// compacts slightly earlier, never later.
pub const IMAGE_TOKEN_ESTIMATE: usize = 1600;

/// Token estimate for one image part, keyed by MIME type.
#[inline]
pub fn estimate_image_tokens(_mime_type: &str) -> usize {
    IMAGE_TOKEN_ESTIMATE
}

/// Ultra-fast token estimation heuristic for Rust.
/// Runs with zero allocations and processes millions of chars per second.
#[inline]
pub fn estimate_token_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_count = 0usize;
    let mut whitespace_count = 0usize;
    let mut punctuation_count = 0usize;
    let mut total_chars = 0usize;

    for ch in text.chars() {
        total_chars += 1;
        let u = ch as u32;

        // CJK Unified Ideographs, Hiragana, Katakana, Hangul
        if (0x4E00..=0x9FFF).contains(&u)
            || (0x3400..=0x4DBF).contains(&u)
            || (0x3040..=0x30FF).contains(&u)
            || (0xAC00..=0xD7AF).contains(&u)
        {
            cjk_count += 1;
        } else if ch.is_whitespace() {
            whitespace_count += 1;
        } else if ch.is_ascii_punctuation()
            || (0x3000..=0x303F).contains(&u)
            || (0xFF00..=0xFFEF).contains(&u)
        {
            // Include ASCII and fullwidth/CJK punctuation
            punctuation_count += 1;
        }
    }

    let ascii_count = total_chars.saturating_sub(cjk_count);
    // Modern BPE tokenizers (OpenAI o200k/cl100k, Claude, DeepSeek) map multi-byte UTF-8 CJK characters
    // to ~1.4 - 1.8 tokens per char on average. ~1.5 tokens avoids 2x under-estimation.
    let cjk_tokens = (cjk_count * 15).div_ceil(10); // ~1.5 tokens per CJK char
    let effective_ascii = ascii_count.saturating_sub(whitespace_count / 2);
    let ascii_tokens = (effective_ascii * 10).div_ceil(37); // ~3.7 chars per token
    let punct_tokens = (punctuation_count * 2).div_ceil(10);

    (cjk_tokens + ascii_tokens + punct_tokens).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        let eng = "Hello world! This is a test sentence for token counting.";
        let eng_tokens = estimate_token_count(eng);
        assert!((10..=20).contains(&eng_tokens));

        let cjk = "你好，世界！这是一个用于 Agent Loop 的测试。";
        let cjk_tokens = estimate_token_count(cjk);
        assert!((20..=40).contains(&cjk_tokens));
    }

    #[test]
    fn test_estimate_image_tokens_is_conservative() {
        assert_eq!(estimate_image_tokens("image/png"), IMAGE_TOKEN_ESTIMATE);
        assert!(estimate_image_tokens("image/jpeg") >= 1000);
    }
}
