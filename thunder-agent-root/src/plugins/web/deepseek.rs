//! DeepSeek's Search and Fetch tools API.
//!
//! Two raw JSON endpoints under the same Bearer key as the chat API:
//!
//! - `POST /v1/tools/search` — DeepSeek's own index. Unlike the keyless
//!   engines in [`super::search`], every hit carries a long `summary` written
//!   for a model to read, a publication date, and an authority score. It also
//!   accepts search operators (`site:`, `lang:`, `after:` …).
//! - `POST /v1/tools/fetch` — server-side rendering that returns the page as
//!   Markdown. It gets past the anti-bot pages the local fetcher is refused by.
//!
//! Both are billed per successful request (5 CNY / 1000), so nothing here runs
//! unless the user opted in through `web.search.provider` / `web.fetch.provider`
//! or `auto` — the defaults stay on the keyless/local paths.
//!
//! The module is deliberately transport-shaped: no tool, no plugin, no registry.
//! Credentials are resolved by [`resolve`] from an injected config section, the
//! provider registry, `auth.json`, and the environment, in that order, so every
//! precedence rule is testable without a network or a real key.

use super::search::SearchResult;
use std::collections::HashMap;
use std::time::Duration;
use thunder_agent_providers::auth::{resolve_env_value, AuthFile};
use thunder_agent_providers::prelude::ProviderRegistry;
use tokio_util::sync::CancellationToken;

/// Default API root. The endpoints live under `/v1`, not at the root itself.
pub const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";
/// Per-request timeout for search.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-request timeout for fetch.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Retries applied to 408 / 429 / 5xx responses when the config names none.
pub const DEFAULT_RETRIES: usize = 1;
/// Delay before a retry. DeepSeek's limit is 5 QPS per account; one short wait
/// is enough for a burst to clear without eating the tool-call budget.
pub const RETRY_BACKOFF: Duration = Duration::from_millis(500);
/// Longest `Retry-After` this client will honour. The kernel's per-tool-call
/// budget is 60s and a fetch may already have spent most of it.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);
/// `query` is documented as 1–128 characters.
pub const MAX_QUERY_CHARS: usize = 128;
/// `count` is documented as 1–50.
pub const MAX_RESULTS: usize = 50;
/// Sent as `User-Agent`, matching the rest of the workspace's fetchers.
pub const USER_AGENT: &str = concat!(
    "Thunder-WebTools/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/lucacicii/thunder)"
);

/// A page fetched by the service, already Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekPage {
    pub title: String,
    pub content: String,
}

/// Resolved credentials + transport for the two endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekConfig {
    pub base_url: String,
    pub api_key: String,
    pub headers: HashMap<String, String>,
    /// Per-request ceiling. `None` keeps each endpoint's own default
    /// ([`SEARCH_TIMEOUT`] / [`FETCH_TIMEOUT`]).
    pub timeout: Option<Duration>,
    pub retries: usize,
}

/// The DeepSeek tools API client.
#[derive(Clone)]
pub struct DeepSeekClient {
    http: reqwest::Client,
    config: DeepSeekConfig,
}

impl std::fmt::Debug for DeepSeekClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the key: this struct is logged through tool errors and
        // tracing fields.
        f.debug_struct("DeepSeekClient")
            .field("base_url", &self.config.base_url)
            .field("api_key", &"<redacted>")
            .field("timeout", &self.config.timeout)
            .field("retries", &self.config.retries)
            .finish()
    }
}

impl DeepSeekClient {
    pub fn new(config: DeepSeekConfig) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            // The per-request timeout is set on each call: search and fetch
            // have different ceilings.
            .build()
            .expect("static reqwest client configuration");
        Self { http, config }
    }

    /// Point the client at another origin (tests, relays). Keeps the real
    /// request/parse path on the wire.
    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.config.base_url = normalize_base_url(base_url);
        self
    }

    pub fn base_url(&self) -> &str {
        &self.config.base_url
    }

    /// `POST /v1/tools/search`.
    ///
    /// `after` is an already-formatted `after:YYYY-MM-DD` operator (see
    /// [`recency_operator`]); it is appended to the query because the API has
    /// no separate recency field.
    ///
    /// `ori_question` is the user's original request. The API documents it as a
    /// hint that improves intent inference — a query the model derived from
    /// "how do I make my crate build faster?" is far more useful to the index
    /// alongside the sentence it came from. It is omitted when the caller has
    /// no user turn (a plugin-initiated search, a test).
    pub async fn search(
        &self,
        query: &str,
        count: usize,
        after: Option<&str>,
        ori_question: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Vec<SearchResult>, String> {
        let query = clamp_query(query, after);
        let mut payload = serde_json::json!({
            "query": query,
            "count": count.clamp(1, MAX_RESULTS),
            // Bodies would be truncated away by the search formatter; the model
            // reads pages with `web_fetch` when a summary is not enough.
            "need_content": false,
        });
        if let Some(question) = ori_question.map(str::trim).filter(|q| !q.is_empty()) {
            payload["ori_question"] = serde_json::Value::String(clamp_question(question));
        }
        let value = self
            .post(
                "/v1/tools/search",
                &payload,
                self.config.timeout.unwrap_or(SEARCH_TIMEOUT),
                cancel,
            )
            .await?;
        Ok(parse_search_results(&value))
    }

    /// `POST /v1/tools/fetch`.
    pub async fn fetch_page(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<DeepSeekPage, String> {
        let payload = serde_json::json!({ "url": url });
        let value = self
            .post(
                "/v1/tools/fetch",
                &payload,
                self.config.timeout.unwrap_or(FETCH_TIMEOUT),
                cancel,
            )
            .await?;
        parse_fetch_response(&value)
    }

    /// One request with the retry policy applied. Returns the parsed body.
    async fn post(
        &self,
        path: &str,
        payload: &serde_json::Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<serde_json::Value, String> {
        let url = format!("{}{}", self.config.base_url, path);
        let mut attempt = 0usize;
        loop {
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err("cancelled".to_string()),
                sent = self
                    .http
                    .post(&url)
                    .timeout(timeout)
                    .header(
                        reqwest::header::AUTHORIZATION,
                        format!("Bearer {}", self.config.api_key),
                    )
                    .headers(headers_from(&self.config.headers))
                    .json(payload)
                    .send() => sent.map_err(|e| format!("request to {path} failed: {e}"))?,
            };

            let status = response.status();
            // Read the header before the body consumes the response.
            let retry_after = retry_after(&response);
            let body = response
                .text()
                .await
                .map_err(|e| format!("failed to read the response from {path}: {e}"))?;

            if status.is_success() {
                return serde_json::from_str::<serde_json::Value>(&body)
                    .map_err(|e| format!("{path} returned a body that is not JSON: {e}"));
            }

            let message = error_message(&body)
                .unwrap_or_else(|| format!("HTTP {} from {path}", status.as_u16()));
            if retryable(status.as_u16()) && attempt < self.config.retries {
                attempt += 1;
                // A 429 usually says how long to wait. Honour it, but never
                // sleep longer than the tool call's own budget allows: past
                // that the caller is better off hearing the failure.
                let wait = retry_after.unwrap_or(RETRY_BACKOFF).min(MAX_RETRY_AFTER);
                tokio::select! {
                    _ = cancel.cancelled() => return Err("cancelled".to_string()),
                    _ = tokio::time::sleep(wait) => {}
                }
                continue;
            }
            return Err(describe_status(status.as_u16(), &message));
        }
    }
}

/// Normalise an API root: drop trailing slashes and a trailing `/v1`, because
/// both endpoint paths already carry `/v1`.
pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let trimmed = trimmed.trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

fn headers_from(map: &HashMap<String, String>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in map {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    headers
}

/// `408`, `429`, and `5xx` are the statuses the API documents as retryable.
fn retryable(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

/// The server's own wait hint, in delta-seconds, capped by the caller. The
/// HTTP-date form is not parsed: an absent or unparseable header yields `None`
/// and the caller falls back to its own backoff.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let raw = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    raw.parse::<u64>().ok().map(Duration::from_secs)
}

/// Read `error.message` out of the OpenAI-compatible error envelope.
pub(crate) fn error_message(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Attach the documented meaning of the status to the service's own message.
fn describe_status(status: u16, message: &str) -> String {
    let hint = match status {
        400 => "invalid request",
        401 => "the DeepSeek API key is missing or invalid",
        402 => "the DeepSeek account has insufficient balance",
        403 => "the fetch was refused by the safety policy",
        404 => "no usable page content could be extracted",
        408 => "the request timed out",
        413 => "the request body is too large",
        429 => "the account's QPS limit was exceeded",
        500 => "internal server error",
        504 => "the upstream request timed out",
        _ => "request failed",
    };
    format!("DeepSeek API {status} ({hint}): {message}")
}

/// Trim `query` to the API's 128-character limit, appending `after` when the
/// room is there. Truncation is by characters, not bytes: a CJK query is
/// counted the way the API counts it.
pub fn clamp_query(query: &str, after: Option<&str>) -> String {
    let query = query.trim();
    let suffix = after.map(|a| format!(" {a}")).unwrap_or_default();
    let budget = MAX_QUERY_CHARS.saturating_sub(suffix.chars().count());
    let kept: String = query.chars().take(budget).collect();
    format!("{kept}{suffix}")
}

/// `ori_question` has the same 128-character limit as `query`, but no operator
/// suffix: it is the user's sentence, not a query.
pub fn clamp_question(question: &str) -> String {
    question.trim().chars().take(MAX_QUERY_CHARS).collect()
}

/// Translate the tool's `recency` enum into an `after:` operator, relative to
/// `now_unix_secs`.
///
/// The API has no recency parameter; it has date operators, so the window is
/// expressed as a lower bound. Windows are approximate by design (a month is
/// 30 days) — the model asked for "recent", not for a calendar boundary.
pub fn recency_operator(recency: &str, now_unix_secs: i64) -> Option<String> {
    let days = match recency.trim().to_ascii_lowercase().as_str() {
        "day" => 1,
        "week" => 7,
        "month" => 30,
        "year" => 365,
        _ => return None,
    };
    let date = civil_date_from_unix(now_unix_secs - days * 86_400);
    Some(format!("after:{date}"))
}

/// `YYYY-MM-DD` for a Unix timestamp (UTC).
pub fn civil_date_from_unix(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The current Unix time in seconds. Split out so the recency tests can pin a
/// clock without touching the system one.
pub fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Days since 1970-01-01 → (year, month, day), in the proleptic Gregorian
/// calendar. Howard Hinnant's `civil_from_days`, which is exact for every date
/// this tool can produce and needs no date crate.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Parse a search reply. Missing/null `results` is an empty result set — the
/// API says a normal search with no hits returns `[]`, and a degraded service
/// must not read as a hard failure.
pub(crate) fn parse_search_results(value: &serde_json::Value) -> Vec<SearchResult> {
    value
        .get("results")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_search_item).collect())
        .unwrap_or_default()
}

fn parse_search_item(item: &serde_json::Value) -> Option<SearchResult> {
    let title = item.get("title").and_then(|v| v.as_str())?.trim();
    let url = item.get("url").and_then(|v| v.as_str())?.trim();
    if title.is_empty() || url.is_empty() {
        return None;
    }
    let snippet = item
        .get("snippet")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let summary = item
        .get("summary")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let date = item
        .get("date")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some(SearchResult {
        title: title.to_string(),
        url: url.to_string(),
        snippet,
        summary,
        date,
    })
}

pub(crate) fn parse_fetch_response(value: &serde_json::Value) -> Result<DeepSeekPage, String> {
    let content = value
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if content.trim().is_empty() {
        return Err("DeepSeek fetch returned no page content".to_string());
    }
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(DeepSeekPage { title, content })
}

/// Resolve credentials for the DeepSeek tools API.
///
/// Precedence, first match wins:
///
/// 1. the `web.deepseek` config section (`apiKey` / `baseUrl`, `${VAR}` expanded);
/// 2. a DeepSeek model already in the provider registry whose `baseUrl` is
///    `api.deepseek.com` itself (a relay's key stays with the relay);
/// 3. `~/.thunder/auth.json`'s `deepseek` entry;
/// 4. `DEEPSEEK_API_KEY`.
///
/// `None` means "no key": the caller then keeps its non-DeepSeek behaviour.
pub fn resolve(
    section: Option<&crate::thunder_config::DeepSeekSection>,
    registry: &ProviderRegistry,
    auth: &AuthFile,
) -> Option<DeepSeekConfig> {
    resolve_with_env(section, registry, auth, |key| std::env::var(key).ok())
}

/// [`resolve`] with an injected environment lookup, so tests never touch the
/// process environment.
pub fn resolve_with_env(
    section: Option<&crate::thunder_config::DeepSeekSection>,
    registry: &ProviderRegistry,
    auth: &AuthFile,
    env: impl Fn(&str) -> Option<String>,
) -> Option<DeepSeekConfig> {
    let configured_key = section
        .and_then(|s| s.api_key.as_deref())
        .and_then(|raw| resolve_env_value(raw).or_else(|| resolve_with(&env, raw)));
    // Borrow a credential from the provider registry only when that entry
    // itself points at DeepSeek's own API. A `deepseek` provider wired to a
    // relay holds the relay's key, and `/v1/tools/*` exists only on
    // `api.deepseek.com` — sending a relay key there would hand a credential to
    // a third party to earn a 401. Such a user configures `web.deepseek` or
    // `DEEPSEEK_API_KEY` instead.
    let registry_spec = registry
        .models
        .iter()
        .find(|m| m.provider == "deepseek" && m.base_url.contains("api.deepseek.com"));
    let registry_key = registry_spec.and_then(|m| m.api_key.clone());

    let api_key = configured_key
        .filter(|k| !k.trim().is_empty())
        .or_else(|| registry_key.filter(|k| !k.trim().is_empty()))
        .or_else(|| auth.api_key_for("deepseek"))
        .or_else(|| env_api_key_with(&env, "deepseek"))?;

    let base_url = section
        .and_then(|s| s.base_url.as_deref())
        .map(|raw| resolve_env_value(raw).unwrap_or_else(|| raw.to_string()))
        .or_else(|| registry_spec.map(|m| m.base_url.clone()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

    Some(DeepSeekConfig {
        base_url: normalize_base_url(&base_url),
        api_key,
        headers: section.and_then(|s| s.headers.clone()).unwrap_or_default(),
        // No configured ceiling: each endpoint keeps its own default. A single
        // value here would stretch a search to the fetch budget.
        timeout: section
            .and_then(|s| s.timeout_ms)
            .map(Duration::from_millis),
        retries: section.and_then(|s| s.retries).unwrap_or(DEFAULT_RETRIES),
    })
}

/// A `${VAR}` / `$VAR` reference that `resolve_env_value` could not satisfy
/// (it reads the real environment and shell rc); the injected lookup is the
/// test seam.
fn resolve_with(env: &impl Fn(&str) -> Option<String>, raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let name = trimmed
        .strip_prefix("${")
        .and_then(|s| s.strip_suffix('}'))
        .or_else(|| trimmed.strip_prefix('$'))?;
    if name.is_empty() {
        return None;
    }
    env(name).filter(|v| !v.trim().is_empty())
}

/// The environment variable that names a DeepSeek key, and nothing else.
///
/// `THUNDER_API_KEY` is deliberately **not** consulted here even though
/// `thunder_agent_providers::auth` treats it as a catch-all: that variable
/// usually holds the key of whatever provider the user runs, and sending an
/// OpenAI or Anthropic key to `api.deepseek.com` would leak a credential to a
/// third party for the sake of an auth error.
fn env_api_key_with(env: &impl Fn(&str) -> Option<String>, provider: &str) -> Option<String> {
    let names: &[&str] = match provider {
        "deepseek" => &["DEEPSEEK_API_KEY"],
        _ => &[],
    };
    for name in names {
        if let Some(v) = env(name).filter(|v| !v.trim().is_empty()) {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thunder_config::DeepSeekSection;
    use thunder_agent_providers::prelude::ModelSpec;

    fn ctx() -> CancellationToken {
        CancellationToken::new()
    }

    /// One-shot HTTP server answering with the queued `(status, body)` pairs.
    /// The request bytes are returned through the handle so a test can assert
    /// on the payload the client actually sent.
    async fn serve(
        responses: Vec<(u16, String)>,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = vec![0u8; 16384];
                let n = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer)
                    .await
                    .unwrap_or(0);
                requests.push(String::from_utf8_lossy(&buffer[..n]).to_string());
                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    401 => "Unauthorized",
                    402 => "Payment Required",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    _ => "Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::flush(&mut stream).await;
            }
            requests
        });
        (base, handle)
    }

    fn client_for(base: &str) -> DeepSeekClient {
        DeepSeekClient::new(DeepSeekConfig {
            base_url: base.to_string(),
            api_key: "sk-test".to_string(),
            headers: HashMap::new(),
            timeout: Some(Duration::from_secs(5)),
            retries: 0,
        })
    }

    fn search_body() -> String {
        serde_json::json!({
            "results": [{
                "rerank_score": 2.4,
                "title": "DeepSeek V4 released",
                "url": "https://example.com/v4",
                "snippet": "short",
                "summary": "long summary",
                "content": "",
                "site_name": "Example",
                "site_icon": null,
                "date": "2026-06-01T00:00:00+00:00",
                "authority": 3
            }]
        })
        .to_string()
    }

    #[test]
    fn base_url_is_normalized_to_the_api_root() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com"
        );
        assert_eq!(
            normalize_base_url("https://relay.test/deepseek"),
            "https://relay.test/deepseek"
        );
        assert_eq!(normalize_base_url("  "), DEFAULT_BASE_URL);
    }

    #[test]
    fn query_is_clamped_and_the_recency_operator_is_appended() {
        let long = "a".repeat(400);
        let clamped = clamp_query(&long, Some("after:2026-01-01"));
        assert_eq!(clamped.chars().count(), MAX_QUERY_CHARS);
        assert!(clamped.ends_with(" after:2026-01-01"));

        let short = clamp_query("  rust  ", None);
        assert_eq!(short, "rust");
    }

    #[test]
    fn recency_maps_to_an_after_operator() {
        // 2026-10-18T00:00:00Z
        let now = 1_792_281_600;
        assert_eq!(
            recency_operator("day", now).as_deref(),
            Some("after:2026-10-17")
        );
        assert_eq!(
            recency_operator("week", now).as_deref(),
            Some("after:2026-10-11")
        );
        assert_eq!(recency_operator("hour", now), None);
    }

    #[test]
    fn civil_dates_are_exact_across_edges() {
        assert_eq!(civil_date_from_unix(0), "1970-01-01");
        assert_eq!(civil_date_from_unix(951_782_400), "2000-02-29");
        assert_eq!(civil_date_from_unix(1_735_603_200), "2024-12-31");
    }

    #[test]
    fn search_reply_parses_into_results() {
        let value: serde_json::Value = serde_json::from_str(&search_body()).unwrap();
        let results = parse_search_results(&value);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "DeepSeek V4 released");
        assert_eq!(results[0].url, "https://example.com/v4");
        assert_eq!(results[0].summary.as_deref(), Some("long summary"));
        assert_eq!(results[0].snippet, "short");
        assert!(results[0].date.is_some());
    }

    #[test]
    fn null_fields_and_empty_results_are_tolerated() {
        let value = serde_json::json!({
            "results": [{
                "title": "T", "url": "https://x.test", "snippet": "",
                "summary": null, "date": null, "site_name": null, "authority": -1
            }]
        });
        let results = parse_search_results(&value);
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.is_none());
        assert!(results[0].date.is_none());

        assert!(parse_search_results(&serde_json::json!({"results": []})).is_empty());
        assert!(parse_search_results(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn fetch_response_requires_content() {
        let page = parse_fetch_response(&serde_json::json!({
            "title": "Example", "content": "# Example"
        }))
        .unwrap();
        assert_eq!(page.title, "Example");
        assert_eq!(page.content, "# Example");

        let err =
            parse_fetch_response(&serde_json::json!({"title": "x", "content": "  "})).unwrap_err();
        assert!(err.contains("no page content"), "{err}");
    }

    #[test]
    fn error_messages_are_read_from_the_envelope() {
        let body = r#"{"error":{"message":"'count' must be between 1 and 50","type":"invalid_request_error","param":null,"code":"invalid_request_error"}}"#;
        assert_eq!(
            error_message(body).as_deref(),
            Some("'count' must be between 1 and 50")
        );
        assert!(error_message("<html>").is_none());
        assert!(describe_status(402, "x").contains("insufficient balance"));
        assert!(describe_status(404, "x").contains("no usable page content"));
    }

    #[tokio::test]
    async fn search_sends_the_documented_payload_and_parses_hits() {
        let (base, server) = serve(vec![(200, search_body())]).await;
        let client = client_for(&base);
        let results = client
            .search(
                "deepseek v4",
                5,
                Some("after:2026-01-01"),
                Some("how fast is v4?"),
                &ctx(),
            )
            .await
            .expect("the endpoint answers");
        let requests = server.await.unwrap();
        assert!(
            requests[0].contains("POST /v1/tools/search"),
            "{}",
            requests[0]
        );
        assert!(requests[0].contains("bearer sk-test") || requests[0].contains("Bearer sk-test"));
        assert!(requests[0].contains("\"count\":5"), "{}", requests[0]);
        assert!(requests[0].contains("\"need_content\":false"));
        assert!(requests[0].contains("after:2026-01-01"));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary.as_deref(), Some("long summary"));
    }

    #[tokio::test]
    async fn fetch_posts_the_url_and_returns_markdown() {
        let (base, server) = serve(vec![(
            200,
            serde_json::json!({"title": "Example", "content": "# Example\n\nBody"}).to_string(),
        )])
        .await;
        let client = client_for(&base);
        let page = client
            .fetch_page("https://example.com/a", &ctx())
            .await
            .expect("the endpoint answers");
        let requests = server.await.unwrap();
        assert!(
            requests[0].contains("POST /v1/tools/fetch"),
            "{}",
            requests[0]
        );
        assert!(requests[0].contains("https://example.com/a"));
        assert_eq!(page.content, "# Example\n\nBody");
    }

    #[tokio::test]
    async fn a_429_is_retried_once_and_then_succeeds() {
        let (base, server) = serve(vec![
            (
                429,
                r#"{"error":{"message":"rate limited","type":"rate_limit","param":null,"code":"rate_limit"}}"#
                    .to_string(),
            ),
            (200, search_body()),
        ])
        .await;
        let mut config = client_for(&base).config;
        config.retries = 1;
        let client = DeepSeekClient::new(config);
        let results = client.search("rust", 3, None, None, &ctx()).await.unwrap();
        assert_eq!(server.await.unwrap().len(), 2);
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn a_402_is_not_retried_and_names_the_cause() {
        let (base, server) = serve(vec![(
            402,
            r#"{"error":{"message":"Insufficient Balance","type":"insufficient_balance"}}"#
                .to_string(),
        )])
        .await;
        let mut config = client_for(&base).config;
        config.retries = 2;
        let client = DeepSeekClient::new(config);
        let err = client
            .search("rust", 3, None, None, &ctx())
            .await
            .unwrap_err();
        assert_eq!(server.await.unwrap().len(), 1, "4xx must not be retried");
        assert!(err.contains("insufficient balance"), "{err}");
        assert!(err.contains("Insufficient Balance"), "{err}");
    }

    #[test]
    fn credentials_resolve_in_the_documented_order() {
        let auth: AuthFile = serde_json::from_str(r#"{"deepseek":"sk-auth"}"#).unwrap();
        let registry = ProviderRegistry::default();

        // 1. The config section wins.
        let section = DeepSeekSection {
            api_key: Some("sk-config".to_string()),
            ..Default::default()
        };
        let resolved = resolve_with_env(Some(&section), &registry, &auth, |_| None).unwrap();
        assert_eq!(resolved.api_key, "sk-config");
        assert_eq!(resolved.base_url, DEFAULT_BASE_URL);

        // 2. auth.json beats the environment.
        let resolved = resolve_with_env(None, &registry, &auth, |k| {
            (k == "DEEPSEEK_API_KEY").then(|| "sk-env".to_string())
        })
        .unwrap();
        assert_eq!(resolved.api_key, "sk-auth");

        // 3. The environment is the last resort.
        let empty = AuthFile::default();
        let resolved = resolve_with_env(None, &registry, &empty, |k| {
            (k == "DEEPSEEK_API_KEY").then(|| "sk-env".to_string())
        })
        .unwrap();
        assert_eq!(resolved.api_key, "sk-env");

        // 4. No key anywhere = no client.
        assert!(resolve_with_env(None, &registry, &empty, |_| None).is_none());

        // A generic THUNDER_API_KEY is not a DeepSeek credential: forwarding
        // another provider's key to api.deepseek.com would leak it.
        assert!(resolve_with_env(None, &registry, &empty, |k| {
            (k == "THUNDER_API_KEY").then(|| "sk-someone-elses".to_string())
        })
        .is_none());
    }

    #[test]
    fn a_registry_deepseek_model_supplies_key_and_base_url() {
        let registry = ProviderRegistry {
            utility_model: None,
            models: vec![ModelSpec {
                provider: "deepseek".to_string(),
                id: "deepseek-chat".to_string(),
                name: "DeepSeek".to_string(),
                api: Default::default(),
                base_url: "https://api.deepseek.com".to_string(),
                api_key: Some("sk-registry".to_string()),
                headers: HashMap::new(),
                reasoning: false,
                input: Vec::new(),
                context_window: 128_000,
                max_tokens: 4096,
                available: true,
                supports_developer_role: false,
                supports_reasoning_effort: false,
                max_tokens_field: "max_tokens".to_string(),
                thinking_levels: vec!["off".to_string()],
                default_thinking_level: "off".to_string(),
                thinking_levels_probed: true,
                thinking_level_map: None,
                compat: None,
                cost: None,
                prompt_cache: None,
            }],
        };
        let resolved = resolve_with_env(None, &registry, &AuthFile::default(), |_| None).unwrap();
        assert_eq!(resolved.api_key, "sk-registry");
        assert_eq!(resolved.base_url, "https://api.deepseek.com");
    }

    #[test]
    fn a_relay_deepseek_model_does_not_lend_its_key() {
        // `deepseek` via a relay holds the relay's credential. The tools API
        // only exists on api.deepseek.com, so borrowing that key would hand it
        // to a third party.
        let registry = ProviderRegistry {
            utility_model: None,
            models: vec![ModelSpec {
                provider: "deepseek".to_string(),
                id: "deepseek-chat".to_string(),
                name: "DeepSeek (relay)".to_string(),
                api: Default::default(),
                base_url: "https://relay.test/v1".to_string(),
                api_key: Some("sk-relay".to_string()),
                headers: HashMap::new(),
                reasoning: false,
                input: Vec::new(),
                context_window: 128_000,
                max_tokens: 4096,
                available: true,
                supports_developer_role: false,
                supports_reasoning_effort: false,
                max_tokens_field: "max_tokens".to_string(),
                thinking_levels: vec!["off".to_string()],
                default_thinking_level: "off".to_string(),
                thinking_levels_probed: true,
                thinking_level_map: None,
                compat: None,
                cost: None,
                prompt_cache: None,
            }],
        };
        assert!(
            resolve_with_env(None, &registry, &AuthFile::default(), |_| None).is_none(),
            "a relay key must not be sent to api.deepseek.com"
        );
    }

    #[test]
    fn an_env_reference_in_the_config_section_is_expanded() {
        let section = DeepSeekSection {
            api_key: Some("${MY_DS_KEY}".to_string()),
            base_url: Some("https://relay.test/ds/v1".to_string()),
            timeout_ms: Some(5_000),
            retries: Some(2),
            headers: None,
        };
        let resolved = resolve_with_env(
            Some(&section),
            &ProviderRegistry::default(),
            &AuthFile::default(),
            |k| (k == "MY_DS_KEY").then(|| "sk-from-env".to_string()),
        )
        .unwrap();
        assert_eq!(resolved.api_key, "sk-from-env");
        assert_eq!(resolved.base_url, "https://relay.test/ds");
        assert_eq!(resolved.timeout, Some(Duration::from_millis(5_000)));
        assert_eq!(resolved.retries, 2);
    }
}
