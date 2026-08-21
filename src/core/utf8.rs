/// UTF-8 safe string slicing helpers to prevent panics on multi-byte character boundaries.

/// Slices string from start to `end` byte index, retreating to the nearest char boundary.
#[inline]
pub fn safe_slice_to(s: &str, mut end: usize) -> &str {
    if end >= s.len() {
        return s;
    }
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Slices string from `start` byte index to the end, advancing to the nearest char boundary.
#[inline]
pub fn safe_slice_from(s: &str, mut start: usize) -> &str {
    if start == 0 {
        return s;
    }
    if start >= s.len() {
        return "";
    }
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_slice_to_ascii() {
        assert_eq!(safe_slice_to("hello world", 5), "hello");
        assert_eq!(safe_slice_to("hello", 100), "hello");
    }

    #[test]
    fn test_safe_slice_to_multibyte_no_panic() {
        let cjk = "你好世界，这是一段中文测试文本";
        // Byte 4 is inside the second CJK char (3 bytes each)
        let sliced = safe_slice_to(cjk, 4);
        assert_eq!(sliced, "你");

        let sliced2 = safe_slice_to(cjk, 2);
        assert_eq!(sliced2, "");
    }

    #[test]
    fn test_safe_slice_from_multibyte_no_panic() {
        let cjk = "你好世界";
        let sliced = safe_slice_from(cjk, 4);
        assert_eq!(sliced, "世界");

        let sliced2 = safe_slice_from(cjk, 2);
        assert_eq!(sliced2, "好世界");
    }
}
