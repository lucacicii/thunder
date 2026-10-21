use regex::Regex;
use std::sync::OnceLock;

/// Strip ANSI terminal escape sequences (colors, cursor movement, formatting) from stderr/stdout (§3.2).
pub fn strip_ansi_codes(input: &str) -> String {
    static ANSI_REGEX: OnceLock<Regex> = OnceLock::new();
    let re = ANSI_REGEX.get_or_init(|| {
        Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*(\x07|\x1b\\)")
            .expect("valid ANSI regex")
    });
    re.replace_all(input, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_color_and_style_escape_codes() {
        let dirty = "\x1b[31;1mError:\x1b[0m file \x1b[33mfoo.rs\x1b[0m not found";
        assert_eq!(strip_ansi_codes(dirty), "Error: file foo.rs not found");
    }
}
