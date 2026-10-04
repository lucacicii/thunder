//! Clickable targets found in rendered output: web URLs and local paths.
//!
//! Everything here comes from model output, so it is untrusted:
//!
//! * only `http` / `https` / `mailto` are opened in a browser — `javascript:`,
//!   `data:` and everything else is dropped;
//! * local paths are *revealed* (`open -R`), never executed, so a path that
//!   happens to be a `.command`, `.app` or script cannot run by being clicked.
//!
//! Argument passing is never shell-mediated, and paths are made absolute before
//! they reach the opener so a name starting with `-` cannot be read as a flag.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where a piece of clickable text points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Opened in the system browser.
    Url(String),
    /// Revealed in the file manager.
    File(PathBuf),
}

impl LinkTarget {
    /// One-line description, for pickers and status messages.
    pub fn display(&self) -> String {
        match self {
            Self::Url(u) => u.clone(),
            Self::File(p) => p.display().to_string(),
        }
    }

    /// `web` / `file`, for picker badges.
    pub fn kind_label(&self) -> &'static str {
        match self {
            Self::Url(_) => "web",
            Self::File(_) => "file",
        }
    }
}

/// Filenames that are paths even without a slash or a dotted extension.
const BARE_NAMES: &[&str] = &[
    "makefile",
    "dockerfile",
    "readme",
    "license",
    "changelog",
    "cargo.lock",
    "justfile",
    "procfile",
];

/// Extensions that make a bare token a path candidate.
const KNOWN_EXTS: &[&str] = &[
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "py",
    "rb",
    "go",
    "java",
    "kt",
    "kts",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "sql",
    "lua",
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "json5",
    "md",
    "mdx",
    "rst",
    "txt",
    "lock",
    "cfg",
    "ini",
    "conf",
    "env",
    "csv",
    "tsv",
    "xml",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    "vue",
    "svelte",
    "gradle",
    "properties",
    "patch",
    "diff",
    "log",
    "plist",
    "pbxproj",
    "xcconfig",
];

/// Caches "does this token name a real file?" decisions.
///
/// `render_chat` rebuilds the whole transcript on every frame (the tick rate is
/// 20 fps), and the existence check is the only filesystem call on that path.
/// Keying by token text keeps it off the redraw loop. Cleared whenever the roots
/// or the session change, since those change the answer.
#[derive(Default)]
pub struct LinkCache {
    resolved: HashMap<String, Option<PathBuf>>,
    limit: usize,
}

impl LinkCache {
    pub fn new() -> Self {
        Self {
            resolved: HashMap::new(),
            limit: 4096,
        }
    }

    /// Drop every memoised decision (workspace, extra roots or session changed).
    pub fn clear(&mut self) {
        self.resolved.clear();
    }

    pub fn len(&self) -> usize {
        self.resolved.len()
    }

    pub fn is_empty(&self) -> bool {
        self.resolved.is_empty()
    }

    fn resolve(&mut self, token: &str, roots: &[PathBuf]) -> Option<PathBuf> {
        if let Some(hit) = self.resolved.get(token) {
            return hit.clone();
        }
        let resolved = resolve_path(token, roots);
        // A crude ceiling: unbounded caching of arbitrary model text is a leak.
        if self.resolved.len() >= self.limit {
            self.resolved.clear();
        }
        self.resolved.insert(token.to_string(), resolved.clone());
        resolved
    }
}

/// Classify one piece of text as a link, or not.
///
/// `roots` are the directories relative paths are resolved against, in order
/// (the workspace first, then any extra roots). A path candidate that does not
/// exist is not a link — that existence check is also what keeps ordinary prose
/// (`and/or`, `a/b`) from lighting up.
pub fn classify(text: &str, roots: &[PathBuf], cache: &mut LinkCache) -> Option<LinkTarget> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }

    if let Some(url) = as_url(raw) {
        return Some(LinkTarget::Url(url));
    }

    let token = clean_token(raw);
    if token.is_empty() {
        return None;
    }

    if let Some(rest) = strip_scheme_ci(&token, "file://") {
        let path = file_url_to_path(rest);
        return path.exists().then_some(LinkTarget::File(path));
    }

    if !looks_like_path(&token) {
        return None;
    }
    cache.resolve(&token, roots).map(LinkTarget::File)
}

/// Whether a token is worth an existence check at all.
fn looks_like_path(token: &str) -> bool {
    if token.is_empty() || token.len() > 1024 {
        return false;
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if token.starts_with('/')
        || token.starts_with("~/")
        || token.starts_with("./")
        || token.starts_with("../")
    {
        return true;
    }
    if token.contains('/') {
        return true;
    }
    let lower = token.to_ascii_lowercase();
    if BARE_NAMES.contains(&lower.as_str()) {
        return true;
    }
    Path::new(token)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| KNOWN_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Trims quoting/bracketing, trailing prose punctuation and a `:line:col`
/// suffix — `src/app.rs:178:9,` is a path, `src/app.rs` is its target.
fn clean_token(text: &str) -> String {
    let mut t = text.trim();

    // Wrapping characters the token may have been quoted or bracketed with.
    loop {
        let trimmed = t
            .trim_start_matches([
                '(', '[', '{', '<', '"', '\'', '`', '“', '‘', '《', '「', '『',
            ])
            .trim_end_matches([
                ')', ']', '}', '>', '"', '\'', '`', ',', '.', ';', '!', '?', '。', '、', '，', ';',
                '：', '）', '】', '》', '」', '』', '”', '’',
            ]);
        if trimmed.len() == t.len() {
            break;
        }
        t = trimmed;
    }

    // `path:12` / `path:12:9` (the convention most tools print).
    t = strip_line_col(t);

    // A trailing bare colon is still punctuation, not part of a filename.
    t.trim_end_matches(':').to_string()
}

/// Removes a trailing `:line` or `:line:col`, leaving the path itself.
fn strip_line_col(text: &str) -> &str {
    let mut end = text.len();
    for _ in 0..2 {
        let Some(colon) = text[..end].rfind(':') else {
            break;
        };
        let digits = &text[colon + 1..end];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        end = colon;
    }
    &text[..end]
}

/// Recognises a URL with an allowed scheme, returning it verbatim.
fn as_url(text: &str) -> Option<String> {
    let lowered = text.to_ascii_lowercase();
    let scheme = ["http://", "https://", "mailto:"]
        .into_iter()
        .find(|s| lowered.starts_with(s))?;

    let rest = &text[scheme.len()..];
    if rest.is_empty() || rest.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    // Trailing sentence punctuation is not part of the URL. `)` and `]` are
    // deliberately kept: `.../Foo_(bar)` is a real URL shape.
    let trimmed = rest.trim_end_matches([',', '.', ';', '!', '?', '\'', '"', '。', '，', '。']);
    if trimmed.is_empty() {
        return None;
    }
    Some(format!("{}{}", &text[..scheme.len()], trimmed))
}

/// Case-insensitive `strip_prefix` for ASCII schemes. `get` keeps this safe on
/// non-ASCII text: `text[..7]` would panic inside a multi-byte character.
fn strip_scheme_ci<'a>(text: &'a str, scheme: &str) -> Option<&'a str> {
    let prefix = text.get(..scheme.len())?;
    if prefix.eq_ignore_ascii_case(scheme) {
        Some(&text[scheme.len()..])
    } else {
        None
    }
}

/// `file:///a/b%20c` -> `/a/b c`.
fn file_url_to_path(rest: &str) -> PathBuf {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    if decoded.starts_with('/') {
        PathBuf::from(decoded)
    } else {
        PathBuf::from(format!("/{decoded}"))
    }
}

/// Minimal `%XX` decoding — enough for spaces and non-ASCII filenames.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &text[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Expands a leading `~` / `~/…` to `$HOME`.
fn expand_home(token: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) => {
            if token == "~" {
                home
            } else if let Some(rest) = token.strip_prefix("~/") {
                format!("{home}/{rest}")
            } else {
                token.to_string()
            }
        }
        Err(_) => token.to_string(),
    }
}

/// Resolves a candidate token to an existing path.
///
/// Absolute paths stand on their own; relative ones are tried against each root
/// in order (workspace first). Returns `None` when nothing exists, which is what
/// makes `classify` conservative.
pub fn resolve_path(token: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let expanded = expand_home(token);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        return path.exists().then_some(path);
    }
    roots
        .iter()
        .map(|root| root.join(&path))
        .find(|candidate| candidate.exists())
}

/// How to reveal a path in the file manager. Pure, so it can be asserted in
/// tests without launching Finder.
pub fn reveal_command(path: &Path) -> (String, Vec<OsString>) {
    #[cfg(target_os = "macos")]
    {
        (
            "open".to_string(),
            vec![OsString::from("-R"), path.as_os_str().to_os_string()],
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No portable "reveal": open the containing directory instead.
        let dir = path.parent().unwrap_or(path);
        ("xdg-open".to_string(), vec![dir.as_os_str().to_os_string()])
    }
}

/// How to open a URL in the browser. Pure, for the same reason.
pub fn open_url_command(url: &str) -> (String, Vec<OsString>) {
    #[cfg(target_os = "macos")]
    let program = "open".to_string();
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open".to_string();
    (program, vec![OsString::from(url)])
}

/// Launch the action for a target, detached. Failures are the caller's to report.
pub fn launch(target: &LinkTarget) -> std::io::Result<()> {
    let (program, args) = match target {
        LinkTarget::Url(u) => open_url_command(u),
        LinkTarget::File(p) => reveal_command(p),
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Every distinct link in a block of text, for the `/links` picker.
///
/// Priority: markdown link targets, then backticked spans, then bare tokens.
/// A bare token that is not a real path is dropped by `classify`.
pub fn extract_links(text: &str, roots: &[PathBuf], cache: &mut LinkCache) -> Vec<LinkTarget> {
    let mut out: Vec<LinkTarget> = Vec::new();
    let mut push = |target: Option<LinkTarget>| {
        if let Some(t) = target {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    };

    for href in markdown_hrefs(text) {
        push(classify(&href, roots, cache));
    }
    for code in backticked_spans(text) {
        push(classify(&code, roots, cache));
    }
    for token in text.split_whitespace() {
        push(classify(token, roots, cache));
    }
    out
}

/// Targets of `[text](target)` and `![alt](target)`.
fn markdown_hrefs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b']' && bytes[i + 1] == b'(' {
            let start = i + 2;
            if let Some(rel_end) = text[start..].find(')') {
                let href = text[start..start + rel_end].trim();
                // Drop an optional title: `(path "title")`.
                let href = href.split_whitespace().next().unwrap_or(href);
                if !href.is_empty() {
                    out.push(href.to_string());
                }
                i = start + rel_end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Contents of inline-code spans.
fn backticked_spans(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            break;
        };
        let span = &after[..close];
        if !span.trim().is_empty() {
            out.push(span.trim().to_string());
        }
        rest = &after[close + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn roots(dir: &Path) -> Vec<PathBuf> {
        vec![dir.to_path_buf()]
    }

    #[test]
    fn urls_open_in_a_browser() {
        let dir = tempdir().unwrap();
        let mut cache = LinkCache::new();
        let r = roots(dir.path());

        assert_eq!(
            classify("https://example.com/a", &r, &mut cache),
            Some(LinkTarget::Url("https://example.com/a".into()))
        );
        assert_eq!(
            classify("http://localhost:3000", &r, &mut cache),
            Some(LinkTarget::Url("http://localhost:3000".into()))
        );
        assert_eq!(
            classify("mailto:me@example.com", &r, &mut cache),
            Some(LinkTarget::Url("mailto:me@example.com".into()))
        );
        // A sentence-final period is punctuation, not part of the URL.
        assert_eq!(
            classify("https://example.com.", &r, &mut cache),
            Some(LinkTarget::Url("https://example.com".into()))
        );
    }

    #[test]
    fn dangerous_schemes_are_rejected() {
        let dir = tempdir().unwrap();
        let mut cache = LinkCache::new();
        let r = roots(dir.path());

        for bad in [
            "javascript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox(1)",
            "ssh://host/x",
            "file:///etc/hosts", // exists, but this is a local path, not a URL
        ] {
            assert!(
                !matches!(classify(bad, &r, &mut cache), Some(LinkTarget::Url(_))),
                "{bad:?} must not be opened as a URL"
            );
        }
    }

    #[test]
    fn file_urls_become_paths() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("a b.txt");
        std::fs::write(&file, "x").unwrap();
        let mut cache = LinkCache::new();

        let url = format!("file://{}", file.display());
        assert_eq!(
            classify(&url, &roots(dir.path()), &mut cache),
            Some(LinkTarget::File(file.clone()))
        );
        // Percent-encoded form decodes to the same path.
        let encoded = format!("file://{}", file.display().to_string().replace(' ', "%20"));
        assert_eq!(
            classify(&encoded, &roots(dir.path()), &mut cache),
            Some(LinkTarget::File(file))
        );
    }

    #[test]
    fn existence_decides_bare_paths() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/cart.py"), "x").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "x").unwrap();
        let mut cache = LinkCache::new();
        let r = roots(dir.path());

        // Relative, resolved against the workspace root.
        assert_eq!(
            classify("src/cart.py", &r, &mut cache),
            Some(LinkTarget::File(dir.path().join("src/cart.py")))
        );
        assert_eq!(
            classify("./Cargo.toml", &r, &mut cache),
            Some(LinkTarget::File(dir.path().join("Cargo.toml")))
        );
        // Absolute.
        let abs = dir.path().join("src/cart.py");
        assert_eq!(
            classify(abs.to_str().unwrap(), &r, &mut cache),
            Some(LinkTarget::File(abs))
        );
        // `:line:col` is stripped.
        assert_eq!(
            classify("src/cart.py:12:3", &r, &mut cache),
            Some(LinkTarget::File(dir.path().join("src/cart.py")))
        );

        // Prose and non-existent paths stay plain text.
        for prose in ["and/or", "a/b", "src/missing.py", "hello", "3/4"] {
            assert_eq!(classify(prose, &r, &mut cache), None, "{prose:?}");
        }
    }

    #[test]
    fn extra_roots_are_searched_after_the_workspace() {
        let ws = tempdir().unwrap();
        let repo = tempdir().unwrap();
        std::fs::write(repo.path().join("main.rs"), "x").unwrap();
        let mut cache = LinkCache::new();

        let all = vec![ws.path().to_path_buf(), repo.path().to_path_buf()];
        assert_eq!(
            classify("main.rs", &all, &mut cache),
            Some(LinkTarget::File(repo.path().join("main.rs")))
        );
        // Workspace wins when both have the file.
        std::fs::write(ws.path().join("main.rs"), "x").unwrap();
        cache.clear();
        assert_eq!(
            classify("main.rs", &all, &mut cache),
            Some(LinkTarget::File(ws.path().join("main.rs")))
        );
    }

    #[test]
    fn reveal_command_is_flag_safe() {
        let (program, args) = reveal_command(Path::new("/tmp/-n"));
        assert_eq!(program, "open");
        // `-R` selects reveal; the path is a single operand after it, so a name
        // that looks like a flag can never be parsed as one.
        assert_eq!(args, vec![OsString::from("-R"), OsString::from("/tmp/-n")]);
    }

    #[test]
    fn url_command_passes_the_url_verbatim() {
        let (program, args) = open_url_command("https://example.com/a b");
        assert_eq!(program, "open");
        assert_eq!(args, vec![OsString::from("https://example.com/a b")]);
    }

    #[test]
    fn cache_is_memoised_and_clears() {
        let dir = tempdir().unwrap();
        let mut cache = LinkCache::new();
        let r = roots(dir.path());

        assert_eq!(classify("new.rs", &r, &mut cache), None);
        assert_eq!(cache.len(), 1);
        // The decision is remembered…
        assert_eq!(classify("new.rs", &r, &mut cache), None);
        assert_eq!(cache.len(), 1);
        // …and forgotten once the roots change.
        std::fs::write(dir.path().join("new.rs"), "x").unwrap();
        assert_eq!(classify("new.rs", &r, &mut cache), None, "still memoised");
        cache.clear();
        assert_eq!(
            classify("new.rs", &r, &mut cache),
            Some(LinkTarget::File(dir.path().join("new.rs")))
        );
    }

    #[test]
    fn extract_links_finds_markdown_code_and_bare_targets() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("app.rs"), "x").unwrap();
        let mut cache = LinkCache::new();
        let r = roots(dir.path());
        let text = "See [docs](https://example.com) and `app.rs` plus bare app.rs and a/b.";

        let links = extract_links(text, &r, &mut cache);
        assert!(links.contains(&LinkTarget::Url("https://example.com".into())));
        assert!(links.contains(&LinkTarget::File(dir.path().join("app.rs"))));
        // Deduplicated: the code span and the bare token are the same file.
        assert_eq!(
            links
                .iter()
                .filter(|l| **l == LinkTarget::File(dir.path().join("app.rs")))
                .count(),
            1
        );
        assert_eq!(
            classify("a/b", &r, &mut cache),
            None,
            "prose must not become a link"
        );
    }
}
