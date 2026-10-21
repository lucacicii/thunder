//! The `web_search` tool: find pages and return sources.
//!
//! Three backends, chained according to `web.search.provider`:
//!
//! - **DeepSeek's tools API** (`/v1/tools/search`) when the user opted in and a
//!   key resolves. It is DeepSeek's own index: every hit carries a long
//!   model-facing summary, a date, and an authority score, and the query
//!   accepts `site:` / `lang:` / `after:` operators. Billed per request, so it
//!   never runs unless the configuration names it or `auto`.
//! - DuckDuckGo's no-JS HTML endpoint (`html.duckduckgo.com/html/`), as a POST.
//!   A GET is challenged outright, and the POST is what the endpoint wants. The
//!   request carries the navigation headers a browser sends; measured on this
//!   machine the endpoint answered `200` with ten parsed results. It also
//!   throttles by IP: after a burst of probing requests the same request shape
//!   started coming back `202` with an `anomaly-modal` challenge page, so the
//!   challenge has to be detected from the body, not from the status code.
//! - Exa's public MCP endpoint (`mcp.exa.ai/mcp`), JSON-RPC over HTTP with an
//!   SSE-framed reply. No API key; the response is `Title: / URL: / Highlights:`
//!   text blocks, so it needs no SDK, only a `data:` line reader. Its free tier
//!   rate-limits by IP too, and says so in `_meta` rather than with a status.
//!
//! The free engines are third-party services, so both can be rate-limited or
//! withdrawn without notice, and both see the query. Neither is a search
//! *index* this crate controls: when every backend fails the tool reports why
//! and the model decides what to do next. The engines that would need a stealth
//! browser (Google, Ecosia, Mojeek) are deliberately absent — thunder ships no
//! browser.
//!
//! `web_fetch` remains the way to read a page; this tool only finds them.

use super::convert::truncate_chars;
use crate::thunder_config::WebSearchBackend;
use async_trait::async_trait;
use html5ever::tendril::TendrilSink;
use html5ever::{parse_document, ParseOpts};
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use serde_json::json;
use std::time::Duration;
use thunder_agent_types::policy::ToolEffect;
use thunder_agent_policy::{AgentTool, ToolExecutionContext};
use thunder_agent_types::tool::ToolDefinition;

/// The tool name exposed to the model.
pub const WEB_SEARCH_TOOL: &str = "web_search";
/// Longest accepted query, in characters.
pub const MAX_QUERY_LENGTH: usize = 400;
/// Hard ceiling on returned results.
pub const MAX_RESULTS: usize = 20;
/// Result count when the model does not ask for one.
pub const DEFAULT_RESULTS: usize = 10;
/// Per-request network timeout. Both backends answer in ~3s; the ceiling is what
/// keeps two failed attempts inside the kernel's 60s per-tool-call budget.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Longest snippet kept per result, in characters.
pub const MAX_SNIPPET_CHARS: usize = 240;
/// Longest DeepSeek summary kept per result, in characters. The service writes
/// 600–800; the cap only guards a runaway reply.
pub const MAX_SUMMARY_CHARS: usize = 1000;

const DDG_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
const EXA_ENDPOINT: &str = "https://mcp.exa.ai/mcp";

/// The UA the engines expect. `USER_AGENT` from `fetch.rs` names thunder and is
/// the honest choice for fetching a page, but it is exactly what a search
/// endpoint filters out, so the search backends send a browser UA instead.
const SEARCH_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

const DESCRIPTION: &str = "Search the web and return ranked sources with titles, URLs, dates, and summaries.\n\n- Use this when you need to find pages, then read the promising ones with `web_fetch`.\n- The backend is chosen by configuration: DeepSeek's own index (when the user enabled it) or the keyless engines (DuckDuckGo first, Exa as a fallback). You do not choose it and do not need to know which one answered.\n- Results are links and summaries, not full answers: fetch a result to read it. When DeepSeek's index answers, each result carries a long model-facing summary; otherwise it carries a short snippet.\n- `query` supports plain keywords and quoted phrases. The DeepSeek index additionally understands search operators, passed through verbatim: `site:github.com`, `-site:example.com`, `-word`, `-\"exact phrase\"`, `\"exact phrase\"`, `lang:zh,en`, `after:2025-01-01`, `before:2026-06-01`. The keyless engines may ignore them.\n- `recency` restricts results to a recent window; it is applied as a date bound where the backend supports one.\n- If every backend fails, the tool reports the per-backend reason instead of returning results.";

const QUERY_DESCRIPTION: &str =
    "The search query. Keywords or a quoted phrase; keep it short and specific.";
const LIMIT_DESCRIPTION: &str = "Maximum number of results to return (default 10, max 20).";
const RECENCY_DESCRIPTION: &str =
    "Restrict results to a recent window: \"day\", \"week\", \"month\", or \"year\".";

/// One search hit, normalized across backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    /// The short blurb the free engines return (100–200 chars).
    pub snippet: String,
    /// DeepSeek's longer, model-facing summary (600–800 chars). `None` for the
    /// free engines, which have nothing comparable.
    pub summary: Option<String>,
    /// Publication date as the backend reported it (RFC 3339 for DeepSeek).
    pub date: Option<String>,
}

impl SearchResult {
    /// A hit from a backend that only reports a short blurb.
    pub fn new(
        title: impl Into<String>,
        url: impl Into<String>,
        snippet: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
            summary: None,
            date: None,
        }
    }
}

/// Which backend produced a result set. Reported in the output so a degraded
/// chain is visible to the model instead of silently looking normal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProvider {
    DeepSeek,
    DuckDuckGo,
    Exa,
}

impl SearchProvider {
    fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::DuckDuckGo => "DuckDuckGo",
            Self::Exa => "Exa",
        }
    }
}

/// Chrome's client hints. The endpoint filters bare API-style requests, so the
/// POST carries the headers a real navigation carries. No cookies are sent or
/// kept, and the block is not tied to a session.
fn browser_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    let pairs = [
        ("sec-ch-ua", r#""Chromium";v="141", "Not?A_Brand";v="24""#),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"macOS\""),
        (
            "accept",
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        ),
        ("accept-language", "en-US,en;q=0.9"),
        ("sec-fetch-dest", "document"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-site", "none"),
        ("sec-fetch-user", "?1"),
        ("upgrade-insecure-requests", "1"),
    ];
    for (name, value) in pairs {
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            reqwest::header::HeaderValue::from_static(value),
        );
    }
    headers
}

/// `df=` accepts only these; anything else is dropped rather than sent.
pub(crate) fn recency_token(recency: &str) -> Option<&'static str> {
    match recency.trim().to_ascii_lowercase().as_str() {
        "day" => Some("d"),
        "week" => Some("w"),
        "month" => Some("m"),
        "year" => Some("y"),
        _ => None,
    }
}

/// True when DuckDuckGo answered with its bot-detection page rather than
/// results. The body arrives with HTTP 200 or 202, so status alone cannot tell.
pub(crate) fn is_ddg_challenge(body: &str) -> bool {
    body.contains("anomaly-modal") || body.contains("anomaly__title")
}

/// Concatenated text of every `<a class="…">` whose class list contains
/// `class_name`, in document order, paired with the `href` of the same anchor.
///
/// The DOM is walked by hand instead of pulling in a CSS-selector crate:
/// `html5ever` is already in the tree as `htmd`'s parser, and the two selectors
/// this tool needs are exact.
fn anchors_with_class(root: &Handle, class_name: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        let mut push_children = true;
        if let NodeData::Element { name, attrs, .. } = &node.data {
            if name.local.as_ref() == "a" {
                let attrs = attrs.borrow();
                let has_class = attrs.iter().any(|a| {
                    a.name.local.as_ref() == "class"
                        && a.value.split_ascii_whitespace().any(|c| c == class_name)
                });
                if has_class {
                    let href = attrs
                        .iter()
                        .find(|a| a.name.local.as_ref() == "href")
                        .map(|a| a.value.to_string())
                        .unwrap_or_default();
                    found.push((href, node_text(&node)));
                    // The snippet anchor wraps `<b>` highlights; those are text
                    // already, and descending again would duplicate it.
                    push_children = false;
                }
            }
        }
        if push_children {
            // Reversed, because the stack is popped from the end: this way the
            // walk visits children in document order.
            for child in node.children.borrow().iter().rev() {
                stack.push(child.clone());
            }
        }
    }
    found
}

/// All text under `node`, whitespace-collapsed.
fn node_text(node: &Handle) -> String {
    let mut out = String::new();
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
        if let NodeData::Text { contents } = &node.data {
            out.push_str(&contents.borrow());
            out.push(' ');
        }
        for child in node.children.borrow().iter().rev() {
            stack.push(child.clone());
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// DuckDuckGo's redirect wrapper carries the real target in `uddg=`.
fn unwrap_ddg_href(href: &str) -> String {
    let Some(query) = href.split_once("uddg=").map(|(_, rest)| rest) else {
        return href.to_string();
    };
    let encoded = query.split('&').next().unwrap_or(query);
    percent_decode(encoded)
}

/// Minimal percent-decoding for the `uddg` payload. Query values arrive as
/// UTF-8 percent-escapes, so invalid escapes and invalid UTF-8 are left as-is
/// rather than failing the whole parse.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
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

/// Parse a DuckDuckGo HTML SERP. Titles and URLs come from the `result__a`
/// anchors; the snippet is the matching `result__snippet` anchor, matched by
/// position because both lists are emitted in result order.
pub(crate) fn parse_ddg(body: &str) -> Vec<SearchResult> {
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(body);
    let titles = anchors_with_class(&dom.document, "result__a");
    let snippets = anchors_with_class(&dom.document, "result__snippet");
    titles
        .into_iter()
        .enumerate()
        .filter_map(|(index, (href, title))| {
            let url = unwrap_ddg_href(&href);
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = snippets
                .get(index)
                .map(|(_, text)| text.clone())
                .unwrap_or_default();
            Some(SearchResult::new(title, url, snippet))
        })
        .collect()
}

/// The JSON-RPC payload of an Exa reply, which may arrive as a plain JSON body
/// or as one `data:` line of an SSE stream.
pub(crate) fn exa_payload(body: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body.trim()) {
        return Some(value);
    }
    for line in body.lines() {
        let Some(raw) = line.strip_prefix("data:") else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw.trim()) {
            return Some(value);
        }
    }
    None
}

/// Exa's text payload: `Title: … URL: … Published: … Author: … Highlights: …`
/// blocks separated by blank lines.
pub(crate) fn parse_exa(text: &str) -> Vec<SearchResult> {
    text.split("\n\n")
        .filter_map(|block| {
            let title = exa_field(block, "Title:")?;
            let url = exa_field(block, "URL:")?;
            let snippet = block
                .split_once("Highlights:")
                .map(|(_, rest)| rest.split_whitespace().collect::<Vec<_>>().join(" "))
                .unwrap_or_default();
            Some(SearchResult::new(title, url, snippet))
        })
        .collect()
}

fn exa_field(block: &str, prefix: &str) -> Option<String> {
    block
        .lines()
        .find_map(|line| line.trim().strip_prefix(prefix))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Render a result set for the model. The backend is named so a degraded chain
/// is visible rather than looking like a normal answer.
pub(crate) fn format_results(
    provider: SearchProvider,
    query: &str,
    results: &[SearchResult],
) -> String {
    let mut out = format!(
        "Web search results for \"{query}\" ({} sources via {}):",
        results.len(),
        provider.label()
    );
    for (index, result) in results.iter().enumerate() {
        // DeepSeek writes its summary for a model to read, so it replaces the
        // 100–200 character blurb the free engines return. The blurb is the
        // fallback: a hit without a summary must not lose its description.
        let (body, cap) = match result.summary.as_deref() {
            Some(summary) if !summary.trim().is_empty() => (summary, MAX_SUMMARY_CHARS),
            _ => (result.snippet.as_str(), MAX_SNIPPET_CHARS),
        };
        let (body, total) = truncate_chars(body, cap);
        let body = if body.is_empty() {
            String::new()
        } else if body.chars().count() < total {
            format!("\n{body}…")
        } else {
            format!("\n{body}")
        };
        let date = result
            .date
            .as_deref()
            .map(|d| format!(" ({})", d.split('T').next().unwrap_or(d)))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n\n[{}] {}{}\n{}{}",
            index + 1,
            result.title,
            date,
            result.url,
            body
        ));
    }
    out
}

/// Clamp a model-supplied `limit` into the range the backends accept.
pub(crate) fn resolve_limit(value: Option<u64>) -> usize {
    let requested = value.unwrap_or(DEFAULT_RESULTS as u64);
    requested.clamp(1, MAX_RESULTS as u64) as usize
}

/// `web_search`, backed by DeepSeek's index when configured, with the free
/// engines (DuckDuckGo, then Exa) as the keyless path.
pub struct WebSearchTool {
    /// One client, so the connection pool is reused across calls. The browser
    /// navigation headers are attached per request — they belong to the
    /// DuckDuckGo form post, not to every call this client makes.
    client: reqwest::Client,
    /// Where the backends live. Production uses the constants; tests point
    /// these at a local server so both branches run without the live engines.
    endpoints: Endpoints,
    /// DeepSeek's tools API. `None` when no credentials resolve; the free
    /// engines then carry every search.
    deepseek: Option<super::deepseek::DeepSeekClient>,
    /// Which chain `execute` walks. `Free` is the default: a key that happens
    /// to exist in the environment must not start billing on its own.
    backend: WebSearchBackend,
    /// The user's turn for this run, sent as DeepSeek's `ori_question`. Only
    /// the DeepSeek backend uses it; the free engines have no equivalent.
    ori_question: Option<String>,
}

#[derive(Debug, Clone)]
struct Endpoints {
    duckduckgo: String,
    exa: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            duckduckgo: DDG_ENDPOINT.to_string(),
            exa: EXA_ENDPOINT.to_string(),
        }
    }
}

impl WebSearchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(SEARCH_USER_AGENT)
            .timeout(SEARCH_TIMEOUT)
            .build()
            .expect("static reqwest client configuration");
        Self {
            client,
            endpoints: Endpoints::default(),
            deepseek: None,
            backend: WebSearchBackend::Free,
            ori_question: None,
        }
    }

    /// Use DeepSeek's index as well. Without a client the tool behaves exactly
    /// as before, so callers can pass `None` unconditionally.
    pub fn with_deepseek(mut self, deepseek: Option<super::deepseek::DeepSeekClient>) -> Self {
        self.deepseek = deepseek;
        self
    }

    /// Select the backend chain (`free` by default).
    pub fn with_backend(mut self, backend: WebSearchBackend) -> Self {
        self.backend = backend;
        self
    }

    /// Attach the user's turn, forwarded to DeepSeek as `ori_question`.
    pub fn with_ori_question(mut self, ori_question: Option<String>) -> Self {
        self.ori_question = ori_question;
        self
    }

    /// Point both free backends at `base` (tests). Keeps the fallback chain and
    /// the parsers on the real code path while removing the network dependency.
    #[cfg(test)]
    fn with_base_url(mut self, base: &str) -> Self {
        self.endpoints = Endpoints {
            duckduckgo: format!("{base}/ddg"),
            exa: format!("{base}/exa"),
        };
        self
    }

    /// DuckDuckGo first: it is a real index, needs no key, and returns titles,
    /// URLs, and snippets.
    async fn duckduckgo(
        &self,
        query: &str,
        limit: usize,
        recency: Option<&str>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<SearchResult>, String> {
        let mut form = vec![("q", query.to_string()), ("kl", "us-en".to_string())];
        if let Some(token) = recency.and_then(recency_token) {
            form.push(("df", token.to_string()));
        }

        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("cancelled".to_string()),
            sent = self
                .client
                .post(&self.endpoints.duckduckgo)
                .headers(browser_headers())
                .form(&form)
                .send() => sent.map_err(|e| format!("request failed: {e}"))?,
        };

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read the response: {e}"))?;
        // A challenge arrives as 200/202 with an anomaly page, so it has to be
        // detected from the body before the status is trusted.
        if is_ddg_challenge(&body) {
            return Err("the endpoint answered with its bot-detection challenge".to_string());
        }
        if !status.is_success() {
            return Err(format!("HTTP {} from the search endpoint", status.as_u16()));
        }

        Ok(parse_ddg(&body).into_iter().take(limit).collect())
    }

    /// Exa's public MCP endpoint. Its reply is SSE-framed JSON-RPC whose
    /// `content[0].text` holds the result blocks.
    async fn exa(
        &self,
        query: &str,
        limit: usize,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<SearchResult>, String> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": query,
                    // The endpoint requires an objective; the query is the one
                    // thing this tool has to describe the goal with.
                    "objective": format!("Find pages that answer: {query}"),
                    "numResults": limit,
                }
            }
        });

        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("cancelled".to_string()),
            sent = self
                .client
                .post(&self.endpoints.exa)
                .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
                .json(&payload)
                .send() => sent.map_err(|e| format!("request failed: {e}"))?,
        };

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| format!("failed to read the response: {e}"))?;
        if !status.is_success() {
            return Err(format!("HTTP {} from the search endpoint", status.as_u16()));
        }

        let value = exa_payload(&body).ok_or("the reply was not JSON")?;
        if let Some(error) = value.get("error") {
            return Err(format!("the endpoint returned an error: {error}"));
        }
        // The free tier answers a throttled call with `_meta.rateLimited` and a
        // "create your own API key" text block, which parses as zero results if
        // it is not recognized for what it is.
        if value
            .pointer("/result/_meta/ai.exa~1rateLimited")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err("the free tier rate-limited this request".to_string());
        }
        let text = value
            .pointer("/result/content/0/text")
            .and_then(|v| v.as_str())
            .ok_or("the reply carried no content")?;

        Ok(parse_exa(text).into_iter().take(limit).collect())
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentTool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new_function(
            WEB_SEARCH_TOOL,
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": QUERY_DESCRIPTION
                    },
                    "limit": {
                        "type": "number",
                        "description": LIMIT_DESCRIPTION
                    },
                    "recency": {
                        "type": "string",
                        "enum": ["day", "week", "month", "year"],
                        "description": RECENCY_DESCRIPTION
                    }
                },
                "required": ["query"]
            }),
        )
    }

    /// A search reads nothing in the workspace and writes nothing: it must be
    /// allowed in plan mode, and no approval mode should nag about it. A host
    /// that wants a gate can still name it in `mode`'s `ask` rules.
    fn effect(&self) -> ToolEffect {
        ToolEffect::Read
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolExecutionContext,
    ) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("Missing 'query' parameter")?
            .trim()
            .to_string();
        if query.is_empty() {
            return Err("Error: the search query is empty.".to_string());
        }
        if query.chars().count() > MAX_QUERY_LENGTH {
            return Err(format!(
                "Error: the search query exceeds the maximum length of {MAX_QUERY_LENGTH} characters."
            ));
        }

        let limit = resolve_limit(args.get("limit").and_then(|v| v.as_u64()));
        let recency = args
            .get("recency")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let cancel = ctx.cancellation_token.clone();

        let mut failures: Vec<String> = Vec::new();

        // `free` skips DeepSeek entirely — an API key in the environment must
        // not start billing because a user happened to export one.
        if matches!(
            self.backend,
            WebSearchBackend::Auto | WebSearchBackend::DeepSeek
        ) {
            match &self.deepseek {
                Some(client) => {
                    let after = recency.as_deref().and_then(|r| {
                        super::deepseek::recency_operator(r, super::deepseek::now_unix_secs())
                    });
                    // The user's sentence helps the index infer intent, but
                    // repeating the query back adds nothing and costs payload:
                    // a search issued with the user's own words as the query
                    // already carries that intent.
                    let ori = self
                        .ori_question
                        .as_deref()
                        .filter(|q| !q.trim().eq_ignore_ascii_case(query.trim()));
                    match client
                        .search(&query, limit, after.as_deref(), ori, &cancel)
                        .await
                    {
                        Ok(results) if !results.is_empty() => {
                            return Ok(format_results(SearchProvider::DeepSeek, &query, &results));
                        }
                        Ok(_) => failures.push("DeepSeek: no results".to_string()),
                        Err(e) => failures.push(format!("DeepSeek: {e}")),
                    }
                }
                // An explicit `deepseek` that cannot authenticate is an error:
                // silently answering from another engine would hide the
                // misconfiguration and make the billed/unbilled distinction
                // invisible.
                None if matches!(self.backend, WebSearchBackend::DeepSeek) => {
                    return Err(
                        "Error: web.search.provider is \"deepseek\" but no DeepSeek API key was found. \
                         Set DEEPSEEK_API_KEY, add a `deepseek` entry to auth.json, or configure web.deepseek.apiKey."
                            .to_string(),
                    );
                }
                None => {}
            }
        }

        match self
            .duckduckgo(&query, limit, recency.as_deref(), &cancel)
            .await
        {
            Ok(results) if !results.is_empty() => {
                return Ok(format_results(SearchProvider::DuckDuckGo, &query, &results));
            }
            Ok(_) => failures.push("DuckDuckGo: no results".to_string()),
            Err(e) => failures.push(format!("DuckDuckGo: {e}")),
        }

        match self.exa(&query, limit, &cancel).await {
            Ok(results) if !results.is_empty() => {
                return Ok(format_results(SearchProvider::Exa, &query, &results));
            }
            Ok(_) => failures.push("Exa: no results".to_string()),
            Err(e) => failures.push(format!("Exa: {e}")),
        }

        Err(format!(
            "Error: every web search backend failed. {}",
            failures.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two results in the shape DuckDuckGo's no-JS endpoint emits, including
    /// the `uddg` redirect wrapper on the first hit.
    const DDG_FIXTURE: &str = r#"
<html><body>
  <div class="result results_links web-result">
    <h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc">Rust Programming Language</a></h2>
    <a class="result__snippet" href="https://rust-lang.org/"><b>Rust</b> is a fast, reliable language.</a>
  </div>
  <div class="result results_links web-result">
    <h2><a rel="nofollow" class="result__a" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">Rust (programming language) - Wikipedia</a></h2>
    <a class="result__snippet" href="https://en.wikipedia.org/wiki/Rust_(programming_language)">Rust is a general-purpose programming language.</a>
  </div>
</body></html>
"#;

    /// A one-shot HTTP/1.1 server: each accepted connection is answered from
    /// `responses` in order with `(status, body)`. Lets the fallback chain run
    /// against the real client and parsers without the live engines.
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
                let reason = if status == 200 { "OK" } else { "Accepted" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\ncontent-type: text/html\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::flush(&mut stream).await;
            }
            served
        });
        (base, handle)
    }

    /// An Exa reply in the shape the live endpoint sends.
    fn exa_sse(text: &str) -> String {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        format!(
            "event: message\ndata: {{\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{escaped}\"}}]}},\"jsonrpc\":\"2.0\",\"id\":1}}\n\n"
        )
    }

    fn ctx() -> ToolExecutionContext {
        ToolExecutionContext {
            tool_call_id: "c1".to_string(),
            turn: 1,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn duckduckgo_hits_are_returned_and_attributed() {
        let (base, server) = serve(vec![(200, DDG_FIXTURE.to_string())]).await;
        let tool = WebSearchTool::new().with_base_url(&base);
        let out = tool
            .execute(
                json!({"query": "rust programming language", "limit": 5}),
                &ctx(),
            )
            .await
            .expect("the first backend answers");
        assert_eq!(server.await.unwrap(), 1, "Exa must not be reached");
        assert!(out.contains("via DuckDuckGo"), "{out}");
        assert!(out.contains("[1] Rust Programming Language"), "{out}");
        assert!(out.contains("https://rust-lang.org/"), "{out}");
        assert!(out.contains("fast, reliable language"), "{out}");
        assert!(!out.contains("uddg="), "the redirect wrapper leaked: {out}");
    }

    #[tokio::test]
    async fn a_challenge_falls_back_to_exa() {
        let challenge =
            r#"<div class="anomaly-modal"><p>Select all squares containing a duck</p></div>"#;
        let (base, server) = serve(vec![
            (202, challenge.to_string()),
            (200, exa_sse("Title: Rust Programming Language\nURL: https://rust-lang.org/\nPublished: N/A\nAuthor: N/A\nHighlights:\nA language empowering everyone.")),
        ])
        .await;
        let tool = WebSearchTool::new().with_base_url(&base);
        let out = tool
            .execute(json!({"query": "rust"}), &ctx())
            .await
            .expect("the fallback answers");
        assert_eq!(server.await.unwrap(), 2, "both backends should be tried");
        assert!(out.contains("via Exa"), "{out}");
        assert!(out.contains("https://rust-lang.org/"), "{out}");
        assert!(!out.contains("anomaly"), "the challenge leaked: {out}");
    }

    #[tokio::test]
    async fn both_backends_failing_reports_each_reason() {
        let challenge = r#"<div class="anomaly-modal">challenge</div>"#;
        let limited = r#"event: message
data: {"result":{"_meta":{"ai.exa/rateLimited":true},"content":[{"type":"text","text":"You've hit Exa's free MCP rate limit."}]},"jsonrpc":"2.0","id":1}

"#;
        let (base, server) = serve(vec![
            (202, challenge.to_string()),
            (200, limited.to_string()),
        ])
        .await;
        let tool = WebSearchTool::new().with_base_url(&base);
        let err = tool
            .execute(json!({"query": "rust"}), &ctx())
            .await
            .expect_err("both backends failed");
        assert_eq!(server.await.unwrap(), 2);
        assert!(err.contains("DuckDuckGo: "), "{err}");
        assert!(err.contains("Exa: "), "{err}");
        assert!(
            err.contains("rate-limited"),
            "the throttle must be named, not reported as no results: {err}"
        );
    }

    /// A DeepSeek client pointed at the mock origin. The mock answers plain
    /// JSON, which is exactly what the real endpoint sends.
    fn deepseek_at(base: &str) -> crate::plugins::web::deepseek::DeepSeekClient {
        use crate::plugins::web::deepseek::{DeepSeekClient, DeepSeekConfig};
        DeepSeekClient::new(DeepSeekConfig {
            base_url: base.to_string(),
            api_key: "sk-test".to_string(),
            headers: Default::default(),
            timeout: Some(std::time::Duration::from_secs(5)),
            retries: 0,
        })
    }

    fn deepseek_reply() -> String {
        json!({
            "results": [{
                "rerank_score": 2.4,
                "title": "Rust Programming Language",
                "url": "https://rust-lang.org/",
                "snippet": "short",
                "summary": "Rust is a language empowering everyone to build reliable and efficient software.",
                "content": "",
                "site_name": "rust-lang.org",
                "site_icon": null,
                "date": "2026-06-01T00:00:00+00:00",
                "authority": 4
            }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn auto_prefers_deepseek_when_it_answers() {
        let (base, server) = serve(vec![(200, deepseek_reply())]).await;
        let tool = WebSearchTool::new()
            .with_base_url(&base)
            .with_deepseek(Some(deepseek_at(&base)))
            .with_backend(WebSearchBackend::Auto);
        let out = tool
            .execute(json!({"query": "rust", "limit": 5}), &ctx())
            .await
            .expect("DeepSeek answers");
        assert_eq!(server.await.unwrap(), 1, "the free engines must not run");
        assert!(out.contains("via DeepSeek"), "{out}");
        assert!(out.contains("empowering everyone"), "{out}");
        assert!(out.contains("(2026-06-01)"), "{out}");
    }

    #[tokio::test]
    async fn the_users_turn_is_sent_as_ori_question() {
        // Captures the request so the payload itself can be asserted: the
        // server-side hint is invisible in the tool's output.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = deepseek_reply();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 16384];
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                .await
                .unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..n]).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            request
        });

        let tool = WebSearchTool::new()
            .with_base_url(&base)
            .with_deepseek(Some(deepseek_at(&base)))
            .with_backend(WebSearchBackend::Auto)
            .with_ori_question(Some("tokio 的调度器是怎么工作的？".to_string()));
        tool.execute(json!({"query": "tokio scheduler"}), &ctx())
            .await
            .expect("DeepSeek answers");

        let request = server.await.unwrap();
        assert!(
            request.contains("\"ori_question\":\"tokio 的调度器是怎么工作的？\""),
            "the user turn should be forwarded: {request}"
        );
        assert!(
            request.contains("\"query\":\"tokio scheduler\""),
            "{request}"
        );
    }

    #[tokio::test]
    async fn a_query_that_is_the_users_turn_does_not_repeat_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let reply = deepseek_reply();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 16384];
            let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                .await
                .unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..n]).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
            request
        });

        let tool = WebSearchTool::new()
            .with_base_url(&base)
            .with_deepseek(Some(deepseek_at(&base)))
            .with_backend(WebSearchBackend::Auto)
            .with_ori_question(Some("tokio scheduler".to_string()));
        tool.execute(json!({"query": "tokio scheduler"}), &ctx())
            .await
            .expect("DeepSeek answers");

        let request = server.await.unwrap();
        assert!(
            !request.contains("ori_question"),
            "repeating the query as the hint costs payload and adds nothing: {request}"
        );
    }

    #[tokio::test]
    async fn auto_falls_back_to_the_free_engines_when_deepseek_is_empty() {
        let (base, server) = serve(vec![
            (200, r#"{"results":[]}"#.to_string()),
            (200, DDG_FIXTURE.to_string()),
        ])
        .await;
        let tool = WebSearchTool::new()
            .with_base_url(&base)
            .with_deepseek(Some(deepseek_at(&base)))
            .with_backend(WebSearchBackend::Auto);
        let out = tool
            .execute(json!({"query": "rust"}), &ctx())
            .await
            .expect("DuckDuckGo answers");
        assert_eq!(server.await.unwrap(), 2);
        assert!(out.contains("via DuckDuckGo"), "{out}");
    }

    #[tokio::test]
    async fn free_never_calls_deepseek_even_with_a_client() {
        let (base, server) = serve(vec![(200, DDG_FIXTURE.to_string())]).await;
        let tool = WebSearchTool::new()
            .with_base_url(&base)
            .with_deepseek(Some(deepseek_at(&base)))
            .with_backend(WebSearchBackend::Free);
        let out = tool
            .execute(json!({"query": "rust"}), &ctx())
            .await
            .expect("DuckDuckGo answers");
        assert_eq!(server.await.unwrap(), 1, "only DuckDuckGo should be asked");
        assert!(out.contains("via DuckDuckGo"), "{out}");
    }

    #[tokio::test]
    async fn an_explicit_deepseek_backend_without_a_key_is_an_error() {
        let tool = WebSearchTool::new().with_backend(WebSearchBackend::DeepSeek);
        let err = tool
            .execute(json!({"query": "rust"}), &ctx())
            .await
            .expect_err("no key configured");
        assert!(err.contains("no DeepSeek API key"), "{err}");
    }
    #[test]
    fn recency_tokens_are_whitelisted() {
        assert_eq!(recency_token("day"), Some("d"));
        assert_eq!(recency_token(" Week "), Some("w"));
        assert_eq!(recency_token("year"), Some("y"));
        assert_eq!(recency_token("hour"), None);
        assert_eq!(recency_token(""), None);
    }

    #[test]
    fn challenge_pages_are_recognized() {
        assert!(is_ddg_challenge(
            r#"<div class="anomaly-modal__mask"></div>"#
        ));
        assert!(!is_ddg_challenge(DDG_FIXTURE));
    }

    #[test]
    fn ddg_serp_yields_titles_urls_and_snippets() {
        let results = parse_ddg(DDG_FIXTURE);
        assert_eq!(results.len(), 2, "both hits should parse: {results:?}");
        assert_eq!(results[0].title, "Rust Programming Language");
        // The redirect wrapper must not leak into the URL the model is told.
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert!(results[0].snippet.contains("fast, reliable language"));
        assert_eq!(
            results[1].url,
            "https://en.wikipedia.org/wiki/Rust_(programming_language)"
        );
        assert!(results[1].snippet.contains("general-purpose"));
    }

    #[test]
    fn limit_is_clamped_into_range() {
        assert_eq!(resolve_limit(None), DEFAULT_RESULTS);
        assert_eq!(resolve_limit(Some(0)), 1);
        assert_eq!(resolve_limit(Some(3)), 3);
        assert_eq!(resolve_limit(Some(500)), MAX_RESULTS);
    }

    #[test]
    fn exa_payload_is_read_from_json_or_sse() {
        let plain = r#"{"jsonrpc":"2.0","id":1,"result":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(exa_payload(plain).is_some());
        let sse = format!("event: message\ndata: {plain}\n\n");
        let value = exa_payload(&sse).expect("the data: line should parse");
        assert_eq!(value.pointer("/result/content/0/text").unwrap(), "x");
        assert!(exa_payload("<html>nope</html>").is_none());
    }

    #[test]
    fn exa_text_blocks_become_results() {
        let text = "Title: Rust Programming Language\nURL: https://rust-lang.org/\nPublished: N/A\nAuthor: N/A\nHighlights:\nA language empowering everyone.\n\nTitle: Rust Documentation\nURL: https://doc.rust-lang.org/stable/\nPublished: N/A\nAuthor: N/A\nHighlights:\nWelcome to an overview of the documentation.";
        let results = parse_exa(text);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Rust Programming Language");
        assert_eq!(results[0].url, "https://rust-lang.org/");
        assert!(results[0].snippet.contains("empowering everyone"));
        assert_eq!(results[1].url, "https://doc.rust-lang.org/stable/");
    }

    #[test]
    fn formatting_names_the_backend_and_truncates_snippets() {
        let results = vec![SearchResult::new(
            "T",
            "https://x.test/a",
            "a".repeat(MAX_SNIPPET_CHARS + 50),
        )];
        let out = format_results(SearchProvider::Exa, "q", &results);
        assert!(out.contains("via Exa"), "{out}");
        assert!(out.contains("[1] T"), "{out}");
        assert!(out.contains("https://x.test/a"), "{out}");
        assert!(out.ends_with('…'), "the snippet should be marked as cut");
    }

    #[test]
    fn a_deepseek_summary_replaces_the_short_snippet() {
        let mut result = SearchResult::new("T", "https://x.test/a", "short blurb");
        result.summary = Some("long model-facing summary ".repeat(10));
        result.date = Some("2026-06-01T00:00:00+00:00".to_string());
        let out = format_results(SearchProvider::DeepSeek, "q", &[result]);
        assert!(out.contains("via DeepSeek"), "{out}");
        assert!(
            out.contains("(2026-06-01)"),
            "the date should be shown: {out}"
        );
        assert!(out.contains("long model-facing summary"), "{out}");
        assert!(
            !out.contains("short blurb"),
            "the summary should win: {out}"
        );
    }

    #[test]
    fn exa_rate_limit_is_not_mistaken_for_no_results() {
        // Captured from the live endpoint: HTTP 200, a `_meta` flag, and a text
        // block that would otherwise parse as an empty result set.
        let sse = "event: message\ndata: {\"result\":{\"_meta\":{\"ai.exa/rateLimited\":true},\"content\":[{\"type\":\"text\",\"text\":\"You've hit Exa's free MCP rate limit.\"}]},\"jsonrpc\":\"2.0\",\"id\":5}\n\n";
        let value = exa_payload(sse).expect("the data: line should parse");
        assert_eq!(
            value
                .pointer("/result/_meta/ai.exa~1rateLimited")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(parse_exa("You've hit Exa's free MCP rate limit.").is_empty());
    }
}
