//! Content-type dispatch, HTML→markdown conversion, and character-budget
//! truncation for `web_fetch`.
//!
//! Everything here is pure: no network, no registry, no async. That is what
//! makes the behaviour testable without a server.

use super::fetch::MAX_MARKDOWN_CHARS;

/// How a response body should be treated before it reaches the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    /// Convert to markdown via `htmd`.
    Html,
    /// Already markdown; pass through.
    Markdown,
    /// Plain text, JSON, XML: pass through untransformed.
    Text,
    /// Not usable as text (PDF, images, archives…).
    Binary,
}

/// Classify a `Content-Type` header value.
///
/// Only the media type before `;` matters — the parameters carry charsets and
/// boundaries, never the shape of the payload. An absent header arrives as `""`
/// and classifies as [`ContentKind::Binary`]: guessing "probably HTML" for a
/// server that said nothing is how a binary blob ends up in the context.
pub fn content_kind(content_type: &str) -> ContentKind {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match mime.as_str() {
        "text/html" | "application/xhtml+xml" => ContentKind::Html,
        "text/markdown" => ContentKind::Markdown,
        "text/plain" | "application/json" | "application/xml" => ContentKind::Text,
        other => {
            if other.starts_with("text/") || other.ends_with("+json") || other.ends_with("+xml") {
                ContentKind::Text
            } else {
                ContentKind::Binary
            }
        }
    }
}

/// Convert an HTML document to markdown.
///
/// `htmd` is Turndown's Rust port — the converter Claude Code uses. Its default
/// handler set treats `<script>`, `<style>`, `<title>` and `<textarea>` as
/// ordinary block elements, which emits their *text* into the markdown, so they
/// are skipped explicitly: a page's inline JavaScript is noise at best and a
/// prompt-injection surface at worst.
///
/// A conversion failure is surfaced instead of silently passing the raw HTML on:
/// the model reads the error and can use `curl` through Bash instead.
pub fn html_to_markdown(html: &str) -> Result<String, String> {
    htmd::HtmlToMarkdown::builder()
        .skip_tags(vec!["script", "style", "title", "textarea"])
        .build()
        .convert(html)
        .map_err(|e| format!("Error: failed to convert HTML to markdown: {e}"))
}

/// Truncate `text` to `max_chars` **characters** (not bytes), returning the kept
/// slice and the original character count.
pub fn truncate_chars(text: &str, max_chars: usize) -> (&str, usize) {
    let total = text.chars().count();
    if total <= max_chars {
        return (text, total);
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    (&text[..end], total)
}

/// The notice appended to a truncated result.
pub fn truncation_notice(total_chars: usize) -> String {
    format!(
        "\n\n[Content truncated due to length; {total_chars} characters total, first {MAX_MARKDOWN_CHARS} processed.]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_markdown_keeps_structure_drops_scripts() {
        let html = r#"<h1>Title</h1><p>Hello <a href="https://x.test/a">link</a></p><script>bad()</script><style>.x{color:red}</style>"#;
        let md = html_to_markdown(html).expect("conversion should succeed");
        assert!(md.contains("# Title"), "heading lost:\n{md}");
        assert!(md.contains("[link](https://x.test/a)"), "link lost:\n{md}");
        assert!(!md.contains("bad()"), "script body survived:\n{md}");
        assert!(!md.contains("color:red"), "style body survived:\n{md}");
    }

    #[test]
    fn content_kind_dispatch() {
        assert_eq!(content_kind("text/html; charset=utf-8"), ContentKind::Html);
        assert_eq!(content_kind("application/xhtml+xml"), ContentKind::Html);
        assert_eq!(content_kind("text/markdown"), ContentKind::Markdown);
        assert_eq!(content_kind("application/json"), ContentKind::Text);
        assert_eq!(
            content_kind("text/plain; charset=iso-8859-1"),
            ContentKind::Text
        );
        assert_eq!(content_kind("application/ld+json"), ContentKind::Text);
        assert_eq!(content_kind("text/csv"), ContentKind::Text);
        assert_eq!(content_kind("application/pdf"), ContentKind::Binary);
        assert_eq!(content_kind(""), ContentKind::Binary);
    }

    #[test]
    fn truncate_chars_reports_total() {
        let long = "a".repeat(200_001);
        let (kept, total) = truncate_chars(&long, MAX_MARKDOWN_CHARS);
        assert_eq!(kept.chars().count(), MAX_MARKDOWN_CHARS);
        assert_eq!(total, 200_001);

        // A multi-byte char straddling the limit must not panic.
        let cjk = "中".repeat(MAX_MARKDOWN_CHARS + 5);
        let (kept, total) = truncate_chars(&cjk, MAX_MARKDOWN_CHARS);
        assert_eq!(kept.chars().count(), MAX_MARKDOWN_CHARS);
        assert_eq!(total, MAX_MARKDOWN_CHARS + 5);
        assert!(kept.ends_with('中'));

        // Under the limit: returned untouched.
        let (kept, total) = truncate_chars("short", MAX_MARKDOWN_CHARS);
        assert_eq!(kept, "short");
        assert_eq!(total, 5);
    }
}
