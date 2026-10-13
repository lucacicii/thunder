//! Process-level cache for `web_fetch` page bodies.
//!
//! The cache is process-wide on purpose: the TUI and the daemon each rebuild
//! their `ThunderRoot` — and therefore the plugin instance — per request, so a
//! cache owned by a plugin instance would never see a second hit. Same reason
//! [`crate::selector::default_selection_cache`] is a static.
//!
//! Entries expire on read (there is no timer thread), and the key is the URL
//! string the caller passed, *before* the `http`→`https` upgrade, so a repeat of
//! the same literal URL hits. Only the fetched body is cached, never the model's
//! answer: a different `prompt` against the same page must still run the model.

use parking_lot::Mutex;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

/// A fetched page, as stored by the cache.
#[derive(Debug, Clone)]
pub struct CachedFetch {
    /// The URL that actually served this body (after redirects).
    pub final_url: String,
    pub status: u16,
    pub reason: String,
    pub content_type: String,
    /// The full decoded body, before markdown conversion and truncation.
    pub text: String,
}

/// Bounded, TTL'd map from request URL to fetched body.
pub struct FetchCache {
    /// Insertion-ordered, oldest first: `put` appends, eviction drops index 0.
    entries: Mutex<Vec<(String, Instant, CachedFetch)>>,
    ttl: Duration,
    max_entries: usize,
}

impl FetchCache {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            ttl,
            max_entries,
        }
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, Vec<(String, Instant, CachedFetch)>> {
        self.entries.lock()
    }

    /// The cached body for `key`, if it is still within the TTL.
    ///
    /// Expired entries are dropped here rather than on a background timer.
    pub fn get(&self, key: &str) -> Option<CachedFetch> {
        let now = Instant::now();
        let mut entries = self.lock();
        entries.retain(|(_, at, _)| now.duration_since(*at) < self.ttl);
        entries
            .iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, _, v)| v.clone())
    }

    /// Store `value` under `key`, evicting the oldest entry when full.
    pub fn put(&self, key: &str, value: CachedFetch) {
        let mut entries = self.lock();
        entries.retain(|(k, _, _)| k != key);
        entries.push((key.to_string(), Instant::now(), value));
        while entries.len() > self.max_entries {
            entries.remove(0);
        }
    }
}

impl std::fmt::Debug for FetchCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let entries = self.lock();
        f.debug_struct("FetchCache")
            .field("entries", &entries.len())
            .field("ttl", &self.ttl)
            .field("max_entries", &self.max_entries)
            .finish()
    }
}

static DEFAULT_FETCH_CACHE: LazyLock<Arc<FetchCache>> = LazyLock::new(|| {
    Arc::new(FetchCache::new(
        super::fetch::CACHE_TTL,
        super::fetch::MAX_CACHE_ENTRIES,
    ))
});

/// The process-wide cache, shared by every `web_fetch` tool instance a host
/// builds. Hosts that need isolation (tests, two embedded roots) inject their
/// own via [`super::WebPlugin::with_cache`].
pub fn global_cache() -> Arc<FetchCache> {
    Arc::clone(&DEFAULT_FETCH_CACHE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> CachedFetch {
        CachedFetch {
            final_url: "https://example.com/".to_string(),
            status: 200,
            reason: "OK".to_string(),
            content_type: "text/html".to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn cache_respects_ttl_and_capacity() {
        // A zero TTL stores nothing usable.
        let expired = FetchCache::new(Duration::ZERO, 2);
        expired.put("a", entry("body"));
        assert!(expired.get("a").is_none(), "zero TTL must not serve hits");

        // Capacity 2: the third insert evicts the oldest, not the newest.
        let bounded = FetchCache::new(Duration::from_secs(60), 2);
        bounded.put("a", entry("first"));
        bounded.put("b", entry("second"));
        bounded.put("c", entry("third"));
        assert!(bounded.get("a").is_none(), "oldest entry should be evicted");
        assert_eq!(bounded.get("b").unwrap().text, "second");
        assert_eq!(bounded.get("c").unwrap().text, "third");

        // Re-putting a key refreshes it rather than duplicating it.
        bounded.put("b", entry("second"));
        assert_eq!(bounded.get("b").unwrap().text, "second");
    }
}
