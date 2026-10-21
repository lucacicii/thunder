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
//! Two backends share that post-processing:
//!
//! - **local** (the default): the `reqwest` fetch described above.
//! - **DeepSeek Fetch** (`/v1/tools/fetch`), opt-in through `web.fetch.provider`:
//!   the service renders the page server-side and returns Markdown, which is
//!   what gets past the anti-bot pages that refuse a plain client. It follows
//!   redirects itself, so the cross-host redirect notice does not appear on
//!   that path. Under `auto` it is a rescue for a local fetch that failed
//!   outright or came back 403/408/429/5xx — never for a 404, and never for a
//!   cross-host redirect, which is a question for the model rather than a
//!   failure.
//!
//! Every user-facing string here — the error wording, the redirect notice, the
//! tool description — is copied from Claude Code 2.1.296's `WebFetch` so a model
//! that learned the tool there behaves the same way here.

use super::cache::{CachedFetch, FetchCache};
use super::convert::{
    content_kind, html_to_markdown, truncate_chars, truncation_notice, ContentKind,
};
use super::summarize::answer_with_utility_model;
use crate::thunder_config::WebFetchBackend;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_providers::prelude::ProviderRegistry;
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_types::tool::ToolDefinition;
use tokio_util::sync::CancellationToken;

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

const DESCRIPTION: &str = "Fetch a URL, convert the page to markdown, and answer `prompt` against it using a small fast model.\n\nIMPORTANT: `web_fetch` WILL FAIL for authenticated or private URLs. Before using this tool, check if the URL points to an authenticated service (e.g. Google Docs, Confluence, Jira, GitHub). If so, look for a specialized MCP tool that provides authenticated access.\n- Fetches content from a specified URL and processes it using an AI model\n- Processes the content with the prompt using a small, fast model\n- Use this tool when you need to retrieve and analyze web content\n- IMPORTANT: If an MCP-provided web fetch tool is available, prefer using that tool instead of this one, as it may have fewer restrictions.\n- localhost and other hostnames without a dot are not supported; for a local server, use curl via Bash\n- The prompt should describe what information you want to extract from the page\n- The backend is chosen by configuration: either the local fetcher or DeepSeek's Fetch API, which renders the page server-side and returns Markdown (it also gets past anti-bot pages that refuse the local fetcher). You do not choose it; results from either backend are interchangeable.\n- With the local fetcher, HTTP is upgraded to HTTPS and cross-host redirects are returned to you rather than followed; call web_fetch again with the redirect URL. DeepSeek's Fetch API follows redirects itself.\n- Includes a self-cleaning cache (entries expire after 15 minutes) for faster responses when repeatedly accessing the same URL\n- For GitHub URLs, prefer using the gh CLI via Bash instead (e.g., gh pr view, gh issue view, gh api).\n- Page content is capped at 100000 characters; when a page is longer the content is truncated and the truncation is reported in the result.";

const ACCEPT_HEADER: &str = "text/markdown, text/html, */*";

/// Whether a local response is worth asking the paid backend about.
///
/// `auto` treats the local fetcher as the default and DeepSeek as a rescue, so
/// only a status the model cannot use — an anti-bot 403, a 429, a 5xx — opens
/// that path. A 404 is excluded: the page genuinely does not exist, and paying
/// to learn the same thing would be waste.
fn worth_remote_retry(status: u16) -> bool {
    matches!(status, 403 | 408 | 429) || (500..600).contains(&status)
}

/// What one network fetch produced.
#[derive(Debug)]
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
    /// DeepSeek's Fetch API, when the user opted in and a key resolves.
    deepseek: Option<super::deepseek::DeepSeekClient>,
    /// Which chain `execute` walks. `Local` is the default: a key that happens
    /// to exist in the environment must not start billing on its own.
    backend: WebFetchBackend,
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
            deepseek: None,
            backend: WebFetchBackend::Local,
        }
    }

    /// Use DeepSeek's Fetch API as well. Without a client the tool behaves
    /// exactly as before, so callers can pass `None` unconditionally.
    pub fn with_deepseek(mut self, deepseek: Option<super::deepseek::DeepSeekClient>) -> Self {
        self.deepseek = deepseek;
        self
    }

    /// Select the backend chain (`local` by default).
    pub fn with_backend(mut self, backend: WebFetchBackend) -> Self {
        self.backend = backend;
        self
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
                let entry = self
                    .fetch_with_backend(&url, &raw_url, &prompt, &ctx.cancellation_token)
                    .await?;
                match entry {
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
    ///
    /// The configured backend decides the order. A cross-host redirect from the
    /// local fetcher is *not* a failure — it is a question for the model — so it
    /// is returned as-is and never triggers the DeepSeek fallback.
    async fn fetch_with_backend(
        &self,
        url: &reqwest::Url,
        raw_url: &str,
        prompt: &str,
        cancel: &CancellationToken,
    ) -> Result<FetchOutcome, String> {
        match self.backend {
            WebFetchBackend::Local => self.fetch(url, raw_url, prompt).await,
            WebFetchBackend::Auto => match self.fetch(url, raw_url, prompt).await {
                Ok(FetchOutcome::Redirect(message)) => Ok(FetchOutcome::Redirect(message)),
                // A 200 (or any page the model can use) is the answer: the
                // paid backend is a rescue, not the default.
                Ok(FetchOutcome::Page(page)) if !worth_remote_retry(page.status) => {
                    Ok(FetchOutcome::Page(page))
                }
                // The anti-bot wall this fallback exists for arrives as a *page*
                // — HTTP 403 with a challenge body — so a non-success status is
                // a failed fetch, not a result to hand back unchanged.
                Ok(FetchOutcome::Page(page)) => match self.deepseek_fetch(url, cancel).await {
                    Ok(Some(remote)) => Ok(FetchOutcome::Page(remote)),
                    // DeepSeek could not do better: the local page still tells
                    // the model what happened, so it is not thrown away.
                    Ok(None) | Err(_) => Ok(FetchOutcome::Page(page)),
                },
                Err(local_err) => match self.deepseek_fetch(url, cancel).await {
                    Ok(Some(page)) => Ok(FetchOutcome::Page(page)),
                    // DeepSeek failed too: report the local failure, because it
                    // is the one the user's configuration is built around.
                    Ok(None) | Err(_) => Err(local_err),
                },
            },
            WebFetchBackend::DeepSeek => match self.deepseek_fetch(url, cancel).await {
                Ok(Some(page)) => Ok(FetchOutcome::Page(page)),
                // An explicit `deepseek` with no key is a misconfiguration, not
                // a reason to silently switch engines.
                Ok(None) => Err(
                    "Error: web.fetch.provider is \"deepseek\" but no DeepSeek API key was found. \
                     Set DEEPSEEK_API_KEY, add a `deepseek` entry to auth.json, or configure web.deepseek.apiKey."
                        .to_string(),
                ),
                Err(remote_err) => match self.fetch(url, raw_url, prompt).await {
                    Ok(FetchOutcome::Redirect(message)) => Ok(FetchOutcome::Redirect(message)),
                    Ok(page) => Ok(page),
                    Err(_) => Err(remote_err),
                },
            },
        }
    }

    /// Fetch through DeepSeek, as a cache entry so the rest of `execute` (the
    /// 100k truncation, the utility-model answer, the markdown fallback) is
    /// shared with the local path.
    ///
    /// `Ok(None)` means no client is configured; `Err` is a real failure.
    async fn deepseek_fetch(
        &self,
        url: &reqwest::Url,
        cancel: &CancellationToken,
    ) -> Result<Option<CachedFetch>, String> {
        let Some(client) = &self.deepseek else {
            return Ok(None);
        };
        let page = client.fetch_page(url.as_str(), cancel).await?;
        Ok(Some(CachedFetch {
            final_url: url.to_string(),
            status: 200,
            reason: "OK".to_string(),
            content_type: "text/markdown".to_string(),
            text: page.content,
        }))
    }

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

    /// A one-shot HTTP server answering `responses` in order, reporting how
    /// many requests it served.
    async fn serve(responses: Vec<(u16, String)>) -> (String, tokio::task::JoinHandle<usize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut served = 0;
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                served += 1;
                let mut buffer = vec![0u8; 8192];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
                let response = format!(
                    "HTTP/1.1 {status} OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::flush(&mut stream).await;
            }
            served
        });
        (base, handle)
    }

    fn deepseek_at(base: &str) -> super::super::deepseek::DeepSeekClient {
        use super::super::deepseek::{DeepSeekClient, DeepSeekConfig};
        DeepSeekClient::new(DeepSeekConfig {
            base_url: base.to_string(),
            api_key: "sk-test".to_string(),
            headers: Default::default(),
            timeout: Some(Duration::from_secs(5)),
            retries: 0,
        })
    }

    /// An origin nothing listens on, so the local fetch fails at connect time.
    fn unreachable() -> reqwest::Url {
        url("http://127.0.0.1:1/page")
    }

    fn tool_with(
        backend: WebFetchBackend,
        deepseek: Option<super::super::deepseek::DeepSeekClient>,
    ) -> WebFetchTool {
        WebFetchTool::new(
            ProviderRegistry::default(),
            Arc::new(FetchCache::new(Duration::from_secs(60), 4)),
        )
        .with_backend(backend)
        .with_deepseek(deepseek)
    }

    #[tokio::test]
    async fn auto_falls_back_to_deepseek_when_the_local_fetch_fails() {
        let (base, server) = serve(vec![(
            200,
            serde_json::json!({"title": "Example", "content": "# Example\n\nBody"}).to_string(),
        )])
        .await;
        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));

        let outcome = tool
            .fetch_with_backend(
                &unreachable(),
                "http://127.0.0.1:1/page",
                "p",
                &CancellationToken::new(),
            )
            .await
            .expect("the fallback answers");
        assert_eq!(server.await.unwrap(), 1);
        match outcome {
            FetchOutcome::Page(page) => {
                assert_eq!(page.text, "# Example\n\nBody");
                assert_eq!(page.status, 200);
            }
            FetchOutcome::Redirect(_) => panic!("expected a page"),
        }
    }

    /// A server that answers any number of requests and counts them, for the
    /// cases where the assertion is "this endpoint was never called". Unlike
    /// [`serve`], it does not block waiting for a request that may never come.
    async fn counting_serve(body: String) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buffer = vec![0u8; 8192];
                let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::flush(&mut stream).await;
            }
        });
        (base, count)
    }

    #[tokio::test]
    async fn a_cross_host_redirect_never_triggers_the_fallback() {
        let (base, deepseek_calls) = counting_serve(
            serde_json::json!({"title": "Example", "content": "should not be used"}).to_string(),
        )
        .await;

        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_base = format!("http://{}", listener.local_addr().unwrap());
        let local = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 8192];
            let _ = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await;
            let response = "HTTP/1.1 301 Moved Permanently\r\nlocation: http://elsewhere.test/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
        });

        let outcome = tool
            .fetch_with_backend(
                &url(&local_base),
                &local_base,
                "what is it?",
                &CancellationToken::new(),
            )
            .await
            .expect("a redirect is an answer, not an error");
        local.await.unwrap();
        assert_eq!(
            deepseek_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "DeepSeek must not be asked when the local fetch produced a redirect"
        );
        match outcome {
            FetchOutcome::Redirect(message) => {
                assert!(message.contains("REDIRECT DETECTED"), "{message}");
                assert!(message.contains("http://elsewhere.test/x"), "{message}");
            }
            FetchOutcome::Page(_) => panic!("the redirect must be reported, not followed"),
        }
    }

    #[tokio::test]
    async fn auto_asks_deepseek_after_a_local_anti_bot_403() {
        let (local, local_server) = serve(vec![(403, "<html>blocked</html>".to_string())]).await;
        let (base, deepseek_calls) = counting_serve(
            serde_json::json!({"title": "Example", "content": "# Rendered by DeepSeek"})
                .to_string(),
        )
        .await;
        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));

        let outcome = tool
            .fetch_with_backend(&url(&local), &local, "p", &CancellationToken::new())
            .await
            .expect("the fallback answers");
        assert_eq!(local_server.await.unwrap(), 1);
        assert_eq!(
            deepseek_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an anti-bot page must trigger the rescue"
        );
        match outcome {
            FetchOutcome::Page(page) => {
                assert_eq!(page.text, "# Rendered by DeepSeek");
                assert_eq!(page.status, 200);
            }
            FetchOutcome::Redirect(_) => panic!("expected a page"),
        }
    }

    #[tokio::test]
    async fn auto_keeps_the_local_page_when_deepseek_cannot_do_better() {
        let (local, _local_server) = serve(vec![(403, "<html>blocked</html>".to_string())]).await;
        let (base, deepseek_calls) =
            counting_serve(r#"{"error":{"message":"no usable page content"}}"#.to_string()).await;
        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));

        let outcome = tool
            .fetch_with_backend(&url(&local), &local, "p", &CancellationToken::new())
            .await
            .expect("the local page is still an answer");
        assert_eq!(deepseek_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        match outcome {
            FetchOutcome::Page(page) => {
                assert_eq!(page.status, 403, "the local status must survive");
                assert!(page.text.contains("blocked"), "{}", page.text);
            }
            FetchOutcome::Redirect(_) => panic!("expected a page"),
        }
    }

    #[tokio::test]
    async fn auto_does_not_pay_for_a_404() {
        let (local, _local_server) = serve(vec![(404, "not found".to_string())]).await;
        let (base, deepseek_calls) = counting_serve(
            serde_json::json!({"title": "Example", "content": "# nope"}).to_string(),
        )
        .await;
        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));

        let outcome = tool
            .fetch_with_backend(&url(&local), &local, "p", &CancellationToken::new())
            .await
            .expect("a 404 is a page the model can read");
        assert_eq!(
            deepseek_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a missing page must not cost a DeepSeek request"
        );
        match outcome {
            FetchOutcome::Page(page) => assert_eq!(page.status, 404),
            FetchOutcome::Redirect(_) => panic!("expected a page"),
        }
    }

    #[tokio::test]
    async fn an_explicit_deepseek_backend_asks_deepseek_first() {
        let (base, server) = serve(vec![(
            200,
            serde_json::json!({"title": "Example", "content": "# From DeepSeek"}).to_string(),
        )])
        .await;
        let tool = tool_with(WebFetchBackend::DeepSeek, Some(deepseek_at(&base)));

        let outcome = tool
            .fetch_with_backend(
                &unreachable(),
                "http://127.0.0.1:1/page",
                "p",
                &CancellationToken::new(),
            )
            .await
            .expect("DeepSeek answers");
        assert_eq!(server.await.unwrap(), 1);
        match outcome {
            FetchOutcome::Page(page) => assert_eq!(page.text, "# From DeepSeek"),
            FetchOutcome::Redirect(_) => panic!("expected a page"),
        }
    }

    #[tokio::test]
    async fn an_explicit_deepseek_backend_without_a_key_is_an_error() {
        let tool = tool_with(WebFetchBackend::DeepSeek, None);
        let err = tool
            .fetch_with_backend(
                &unreachable(),
                "http://127.0.0.1:1/page",
                "p",
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.contains("no DeepSeek API key"), "{err}");
    }

    #[tokio::test]
    async fn auto_reports_the_local_failure_when_deepseek_fails_too() {
        let (base, server) = serve(vec![(
            404,
            r#"{"error":{"message":"no usable content"}}"#.to_string(),
        )])
        .await;
        let tool = tool_with(WebFetchBackend::Auto, Some(deepseek_at(&base)));
        let err = tool
            .fetch_with_backend(
                &unreachable(),
                "http://127.0.0.1:1/page",
                "p",
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(server.await.unwrap(), 1);
        assert!(
            err.contains("127.0.0.1:1"),
            "the local failure should be reported: {err}"
        );
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
