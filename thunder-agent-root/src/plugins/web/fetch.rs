//! The `web_fetch` tool: fetch a URL, convert it to markdown, answer a prompt
//! against it.
//!
//! Semantics mirror Claude Code's `WebFetch`, minus the parts that need
//! machinery thunder does not have (preapproved-domain prompt variants, writing
//! oversized pages to a scratch file). What survives is the behaviour that
//! matters to the model: `http` is upgraded to `https`, a cross-host redirect is
//! reported back rather than followed, binary content is refused, and the answer
//! comes from the utility model with the markdown as a fallback.
//!
//! Every user-facing string here — the error wording, the redirect notice, the
//! tool description — is copied from Claude Code 2.1.296's `WebFetch` so a model
//! that learned the tool there behaves the same way here.

use super::cache::{CachedFetch, FetchCache};
use super::convert::{
    content_kind, html_to_markdown, truncate_chars, truncation_notice, ContentKind,
};
use super::summarize::answer_with_utility_model;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_loop::types::policy::ToolEffect;
use thunder_agent_loop::types::tool::{AgentTool, ToolDefinition, ToolExecutionContext};
use thunder_agent_providers::prelude::ProviderRegistry;

/// The tool name exposed to the model.
pub const WEB_FETCH_TOOL: &str = "web_fetch";

/// Longest URL accepted, in characters.
pub const MAX_URL_LENGTH: usize = 2000;
/// Longest page body handed to the model, in characters.
pub const MAX_MARKDOWN_CHARS: usize = 100_000;
/// How many same-host redirects to follow before giving up.
pub const MAX_REDIRECTS: usize = 10;
/// How many fetched pages to keep in the process-level cache.
pub const MAX_CACHE_ENTRIES: usize = 32;
/// Per-request network timeout.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout for the utility-model summarisation call.
pub const SUMMARIZE_TIMEOUT_MS: u64 = 30_000;
/// Token ceiling for the summary.
pub const MAX_SUMMARY_TOKENS: usize = 2048;
/// Cache lifetime. Twenty seconds of fetch plus thirty of summarisation stays
/// inside the kernel's default 60s per-tool-call budget
/// (`AgentConfig::request_timeout_ms`).
pub const CACHE_TTL: Duration = Duration::from_secs(900);
/// Sent as `User-Agent`: a fetcher that does not identify itself is treated as
/// a scraper, and some sites answer 403 to `reqwest`'s default.
pub const USER_AGENT: &str = concat!(
    "Thunder-WebFetch/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/lucacicii/thunder)"
);

const DESCRIPTION: &str = "Fetch a URL, convert the page to markdown, and answer `prompt` against it using a small fast model.\n\nIMPORTANT: `web_fetch` WILL FAIL for authenticated or private URLs. Before using this tool, check if the URL points to an authenticated service (e.g. Google Docs, Confluence, Jira, GitHub). If so, look for a specialized MCP tool that provides authenticated access.\n- Fetches content from a specified URL and processes it using an AI model\n- Processes the content with the prompt using a small, fast model\n- Use this tool when you need to retrieve and analyze web content\n- IMPORTANT: If an MCP-provided web fetch tool is available, prefer using that tool instead of this one, as it may have fewer restrictions.\n- localhost and other hostnames without a dot are not supported; for a local server, use curl via Bash\n- The prompt should describe what information you want to extract from the page\n- HTTP is upgraded to HTTPS. Cross-host redirects are returned to you rather than followed; call web_fetch again with the redirect URL.\n- Includes a self-cleaning cache (entries expire after 15 minutes) for faster responses when repeatedly accessing the same URL\n- For GitHub URLs, prefer using the gh CLI via Bash instead (e.g., gh pr view, gh issue view, gh api).\n- Page content is capped at 100000 characters; when a page is longer the content is truncated and the truncation is reported in the result.";

const ACCEPT_HEADER: &str = "text/markdown, text/html, */*";

/// What one network fetch produced.
enum FetchOutcome {
    /// A page body (HTML already converted), with its status and content type.
    Page(CachedFetch),
    /// A redirect the tool will not follow, rendered as the message the model
    /// gets back. Not an error: retrying is the model's call to make.
    Redirect(String),
}

/// `web_fetch`, backed by the registry's utility model.
pub struct WebFetchTool {
    registry: ProviderRegistry,
    cache: Arc<FetchCache>,
    /// Built once: a `reqwest::Client` owns a connection pool, and redirects are
    /// handled by hand so a cross-host hop can be reported to the model.
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new(registry: ProviderRegistry, cache: Arc<FetchCache>) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("static reqwest client configuration");
        Self {
            registry,
            cache,
            client,
        }
    }
}

#[async_trait]
impl AgentTool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            WEB_FETCH_TOOL,
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch content from"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt to run on the fetched content"
                    }
                },
                "required": ["url", "prompt"]
            }),
        )
    }

    /// Reading a web page changes nothing in the workspace, and looking
    /// something up is legitimate work in plan mode. An approval mode that
    /// inherits a gate can still require approval for this tool by name
    /// (`mode`'s `ask` rules), but the default classification must not be the
    /// catch-all `Other` a tool with no declared effect would get.
    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let raw_url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'url' parameter")?
            .to_string();
        let prompt = args
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'prompt' parameter")?
            .to_string();

        // The cache is keyed by the URL as the model wrote it, so a repeat of the
        // same call costs no network. A different `prompt` still re-runs the
        // model below: only the page is cached, never the answer.
        let fetched = match self.cache.get(&raw_url) {
            Some(hit) => hit,
            None => {
                let (url, cache_key) = prepare_url(&raw_url)?;
                match self.fetch(&url, &raw_url, &prompt).await? {
                    // A redirect that needs the model's decision produced no
                    // page, so nothing is cached: the retry arrives under the
                    // redirect target's own key.
                    FetchOutcome::Redirect(message) => return Ok(message),
                    FetchOutcome::Page(entry) => {
                        self.cache.put(&cache_key, entry.clone());
                        entry
                    }
                }
            }
        };

        let (content, total_chars) = truncate_chars(&fetched.text, MAX_MARKDOWN_CHARS);
        let notice = if content.chars().count() < total_chars {
            truncation_notice(total_chars)
        } else {
            String::new()
        };

        let answer = answer_with_utility_model(
            &self.registry,
            &fetched.final_url,
            content,
            &prompt,
            ctx.cancellation_token.clone(),
        )
        .await
        .ok();

        let mut out = match answer {
            Some(a) if !a.trim().is_empty() => a,
            _ => content.to_string(),
        };
        if fetched.status >= 400 {
            out = format!("[HTTP {} {}]\n\n{out}", fetched.status, fetched.reason);
        }
        out.push_str(&notice);
        Ok(out)
    }
}

impl WebFetchTool {
    /// Fetch `url`, following same-host redirects and reporting the rest.
    ///
    /// `raw_url` only names the URL in the "too many redirects" error; `url` is
    /// the upgraded one that is actually requested.
    async fn fetch(
        &self,
        url: &reqwest::Url,
        raw_url: &str,
        prompt: &str,
    ) -> Result<FetchOutcome, String> {
        let original_url = url.clone();
        let mut current = url.clone();
        let mut response = None;

        for _ in 0..MAX_REDIRECTS {
            let resp = self
                .client
                .get(current.clone())
                .header("accept", ACCEPT_HEADER)
                .send()
                .await
                .map_err(|e| format!("Error: failed to fetch {current}: {e}"))?;

            let status = resp.status();
            if status.is_redirection() {
                let location = resp
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or("Redirect missing Location header")?
                    .to_string();
                let next = reqwest::Url::parse(&location)
                    .or_else(|_| current.join(&location))
                    .map_err(|e| format!("Error: invalid redirect target \"{location}\": {e}"))?;

                if !redirect_allowed(&current, &next) {
                    // Not an error: the model must decide, and to decide it needs
                    // the target URL and the arguments to retry with.
                    return Ok(FetchOutcome::Redirect(redirect_message(
                        &original_url,
                        &next,
                        status.as_u16(),
                        status.canonical_reason().unwrap_or(""),
                        prompt,
                    )));
                }

                current = next;
                continue;
            }

            response = Some((resp, status));
            break;
        }

        let (resp, status) = response.ok_or_else(|| {
            format!("Too many redirects (exceeded {MAX_REDIRECTS}) for {raw_url}")
        })?;

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let kind = content_kind(&content_type);
        if kind == ContentKind::Binary {
            return Err(format!(
                "Error: web_fetch cannot process binary content (content-type: {content_type})"
            ));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| format!("Error: failed to read the response body from {current}: {e}"))?;
        if body.trim().is_empty() {
            return Err(format!(
                "Error: the server returned an empty body for {current}"
            ));
        }

        let text = match kind {
            ContentKind::Html => html_to_markdown(&body)?,
            ContentKind::Markdown | ContentKind::Text => body,
            ContentKind::Binary => unreachable!("binary content returned above"),
        };

        Ok(FetchOutcome::Page(CachedFetch {
            final_url: current.to_string(),
            status: status.as_u16(),
            reason: status.canonical_reason().unwrap_or("").to_string(),
            content_type,
            text,
        }))
    }
}

/// Validate a caller-supplied URL, upgrade it to `https`, and return it with the
/// cache key to use.
///
/// The cache key is the URL as written, *before* the upgrade: the model repeats
/// the URL it was given, and the caller-supplied string is the only key both
/// calls share.
pub(crate) fn prepare_url(raw: &str) -> Result<(reqwest::Url, String), String> {
    if raw.chars().count() > MAX_URL_LENGTH {
        return Err(format!(
            "Error: Invalid URL. The URL exceeds the maximum length of {MAX_URL_LENGTH} characters."
        ));
    }

    let mut url = reqwest::Url::parse(raw).map_err(|_| {
        format!("Error: Invalid URL \"{raw}\". The URL provided could not be parsed.")
    })?;

    match url.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(format!(
                "Error: Invalid URL \"{raw}\". Only http and https URLs are supported."
            ))
        }
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "Error: Invalid URL \"{raw}\". URLs with embedded credentials are not supported."
        ));
    }

    let host = url.host_str().unwrap_or("");
    if !host.contains('.') {
        return Err(format!(
            "Error: Invalid URL \"{raw}\". localhost and other hostnames without a dot are not supported; for a local server, use curl via bash."
        ));
    }

    if url.scheme() == "http" {
        // Upgrading costs nothing when the site serves TLS — which is nearly
        // always — and refusing to downgrade is the safer default.
        url.set_scheme("https")
            .expect("http to https is a valid scheme change");
    }

    Ok((url, raw.to_string()))
}

/// Host without a leading `www.`, for redirect comparison.
fn strip_leading_www(host: Option<&str>) -> &str {
    let host = host.unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host)
}

/// Whether a redirect may be followed without asking the model.
///
/// Same scheme, same effective port, same host (modulo a `www.` prefix), and no
/// credentials smuggled into the target. Anything else — in particular a
/// different registrable domain — is reported instead: following it would let
/// one host's page redirect the fetch to another host's content under the
/// original URL's authority.
pub(crate) fn redirect_allowed(from: &reqwest::Url, to: &reqwest::Url) -> bool {
    from.scheme() == to.scheme()
        && from.port_or_known_default() == to.port_or_known_default()
        && strip_leading_www(from.host_str()) == strip_leading_www(to.host_str())
        && to.username().is_empty()
        && to.password().is_none()
}

/// The result handed back to the model when a redirect needs its decision.
pub(crate) fn redirect_message(
    original: &reqwest::Url,
    redirect: &reqwest::Url,
    status: u16,
    reason: &str,
    prompt: &str,
) -> String {
    format!(
        "REDIRECT DETECTED: The URL redirects to a different host.\n\nOriginal URL: {original}\nRedirect URL: {redirect}\nStatus: {status} {reason}\n\nTo complete your request, I need to fetch content from the redirected URL. Please use web_fetch again with these parameters:\n- url: \"{redirect}\"\n- prompt: \"{prompt}\""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn prepare_url_upgrades_http_and_keeps_original_key() {
        let (parsed, key) = prepare_url("http://example.com/a?b=c").unwrap();
        assert_eq!(parsed.as_str(), "https://example.com/a?b=c");
        assert_eq!(key, "http://example.com/a?b=c");
    }

    #[test]
    fn prepare_url_refuses_dotless_and_userinfo_and_ftp() {
        let err = prepare_url("http://localhost:3000/x").unwrap_err();
        assert!(err.contains("hostnames without a dot"), "{err}");

        let err = prepare_url("https://user:pw@example.com/x").unwrap_err();
        assert!(err.contains("embedded credentials"), "{err}");

        let err = prepare_url("ftp://example.com/x").unwrap_err();
        assert!(err.contains("Only http and https"), "{err}");

        let err = prepare_url("not a url").unwrap_err();
        assert!(err.contains("could not be parsed"), "{err}");

        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LENGTH));
        let err = prepare_url(&long).unwrap_err();
        assert!(err.contains("exceeds the maximum length"), "{err}");
    }

    #[test]
    fn redirect_allowed_is_same_host_only() {
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("https://b.test/y")
        ));
        assert!(redirect_allowed(
            &url("https://www.a.test/x"),
            &url("https://a.test/y")
        ));
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("http://a.test/y")
        ));
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("https://u:p@a.test/y")
        ));
        assert!(!redirect_allowed(
            &url("https://a.test/x"),
            &url("https://a.test:8443/y")
        ));
        assert!(redirect_allowed(
            &url("https://a.test:443/x"),
            &url("https://a.test/y")
        ));
    }

    #[test]
    fn redirect_message_names_the_retry_arguments() {
        let msg = redirect_message(
            &url("https://a.test/x"),
            &url("https://b.test/y"),
            301,
            "Moved Permanently",
            "what is on the page?",
        );
        assert!(msg.starts_with("REDIRECT DETECTED: The URL redirects to a different host."));
        assert!(msg.contains("Original URL: https://a.test/x"));
        assert!(msg.contains("Redirect URL: https://b.test/y"));
        assert!(msg.contains("Status: 301 Moved Permanently"));
        assert!(msg.contains("- url: \"https://b.test/y\""));
        assert!(msg.contains("- prompt: \"what is on the page?\""));
    }
}
