//! High-performance string & binary sanitizer for LLM tool outputs.
//! Strips ANSI color/control codes and prevents binary context poisoning.

/// Strips ANSI escape sequences (e.g. `\x1b[31m`, `\x1b[0m`, CSI codes)
pub fn strip_ansi_escapes(input: &str) -> String {
    if !input.contains('\x1b') {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if let Some(&next) = chars.peek() {
                if next == '[' {
                    // CSI sequence: ESC [ ... [a-zA-Z]
                    chars.next(); // consume '['
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() || c == 'm' || c == 'K' || c == 'H' || c == 'J' {
                            break;
                        }
                    }
                    continue;
                } else if next == ']' {
                    // OSC sequence: ESC ] ... (BEL or ESC \)
                    chars.next(); // consume ']'
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c == '\x07' || c == '\x1b' {
                            if c == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                    continue;
                } else if next == '(' || next == ')' {
                    chars.next();
                    chars.next();
                    continue;
                }
            }
        }
        result.push(ch);
    }

    result
}

/// Checks if byte slice looks like binary data rather than valid text
pub fn is_binary_data(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }

    // Check first 4096 bytes for quick heuristic
    let sample = &bytes[..bytes.len().min(4096)];
    let mut null_count = 0;
    let mut control_count = 0;

    for &b in sample {
        if b == 0 {
            null_count += 1;
        } else if b < 32 && b != b'\n' && b != b'\r' && b != b'\t' {
            control_count += 1;
        }
    }

    // Null bytes are almost certain indicators of binary content (ELF, images, etc.)
    if null_count > 0 {
        return true;
    }

    // High proportion of unusual control characters
    (control_count as f64) / (sample.len() as f64) > 0.08
}

/// Sanitizes tool output: cleans ANSI sequences and guards against binary pollution
pub fn sanitize_tool_output(output: String) -> String {
    let bytes = output.as_bytes();
    if is_binary_data(bytes) {
        return format!("[Binary data omitted: {} bytes]", bytes.len());
    }

    strip_ansi_escapes(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_escapes() {
        let colored = "\x1b[31;1mError:\x1b[0m File not found \x1b[32m[OK]\x1b[0m";
        assert_eq!(strip_ansi_escapes(colored), "Error: File not found [OK]");

        let plain = "Clean text without ansi";
        assert_eq!(strip_ansi_escapes(plain), plain);
    }

    #[test]
    fn test_binary_detection() {
        let binary_payload = vec![0x7f, b'E', b'L', b'F', 0, 1, 1, 0, 0, 0, 0];
        assert!(is_binary_data(&binary_payload));

        let text_payload = b"Hello, this is regular UTF-8 text.\nLine 2\tIndented\r\n";
        assert!(!is_binary_data(text_payload));
    }

    #[test]
    fn test_sanitize_tool_output() {
        let colored = "\x1b[33mWarning:\x1b[0m check disk";
        assert_eq!(sanitize_tool_output(colored.to_string()), "Warning: check disk");

        let binary_str = String::from_utf8_lossy(&[0, 1, 2, 3, 0, 5]).to_string();
        let sanitized = sanitize_tool_output(binary_str);
        assert!(sanitized.contains("[Binary data omitted"));
    }
}
