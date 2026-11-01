//! In-memory HTTP response cache, ported from `searx/cache.py`.
//!
//! Upstream configuration equivalents are `cache.ttl` (default 300 s) and
//! `cache.max_bytes` (default 8 MB). Eviction preserves insertion order plus TTL;
//! reads intentionally do not refresh order or expiry (compatibility behavior).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Cached response payload (a clone of the parts engines look at).
#[derive(Debug, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: std::sync::Arc<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub hit: u64,
    pub miss: u64,
    pub entries: usize,
    pub bytes: usize,
    pub error: u64,
}

struct Entry {
    stored: Instant,
    response: CachedResponse,
    size: usize,
    derived_page_text: Option<DerivedPageText>,
    // Links share key allocations with the map instead of copying URL strings.
    previous: Option<Arc<str>>,
    next: Option<Arc<str>>,
}

/// A single rendering of this exact cached body. Keeping it on the raw entry
/// gives it the same generation, lifetime, eviction and clearing behavior.
struct DerivedPageText {
    max_chars: usize,
    text: Option<std::sync::Arc<str>>,
    accounted_bytes: usize,
}

/// Larger outputs are still returned in full; they simply are not retained.
const MAX_DERIVED_PAGE_TEXT_BYTES: usize = 64 * 1024;
/// Conservative additional accounting for the derived slot and Arc metadata.
const DERIVED_PAGE_TEXT_OVERHEAD: usize = 128;

#[derive(Default)]
struct Inner {
    entries: HashMap<Arc<str>, Entry>,
    oldest: Option<Arc<str>>,
    newest: Option<Arc<str>>,
    bytes: usize,
    derived_bytes: usize,
}

/// A bounded TTL + insertion-order response cache. Key removal is expected O(1).
pub struct ResponseCache {
    inner: Mutex<Inner>,
    ttl: Duration,
    max_bytes: usize,
    hit: AtomicU64,
    miss: AtomicU64,
    error: AtomicU64,
}

impl Default for ResponseCache {
    fn default() -> Self {
        Self::new(Duration::from_secs(300), 8 * 1024 * 1024)
    }
}

impl ResponseCache {
    pub fn new(ttl: Duration, max_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            ttl,
            max_bytes,
            hit: AtomicU64::new(0),
            miss: AtomicU64::new(0),
            error: AtomicU64::new(0),
        }
    }

    /// The cache key of a request: method + URL + body.
    pub fn key(method: &str, url: &str, body: &str) -> String {
        format!("{method} {url} {body}")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn get(&self, key: &str) -> Option<CachedResponse> {
        let mut inner = self.lock();
        let expired = inner
            .entries
            .get(key)
            .map(|entry| entry.stored.elapsed() > self.ttl)
            .unwrap_or(false);
        if expired {
            self.remove_locked(&mut inner, key);
            self.miss.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        if let Some(entry) = inner.entries.get(key) {
            let response = entry.response.clone();
            self.hit.fetch_add(1, Ordering::Relaxed);
            Some(response)
        } else {
            self.miss.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Read page text from the same raw-response cache entry. The outer None
    /// means the caller must use its normal fetch path; Some(None) is a cached
    /// body with no readable text. A miss here deliberately does not increment
    /// the miss counter: the following Network::fetch owns that lookup.
    fn page_text(&self, key: &str, max_chars: usize) -> Option<Option<String>> {
        let (body, stored) = {
            let mut inner = self.lock();
            let expired = inner
                .entries
                .get(key)
                .map(|entry| entry.stored.elapsed() > self.ttl)
                .unwrap_or(false);
            if expired {
                self.remove_locked(&mut inner, key);
                return None;
            }
            let entry = inner.entries.get(key)?;
            self.hit.fetch_add(1, Ordering::Relaxed);
            if let Some(derived) = &entry.derived_page_text {
                if derived.max_chars == max_chars {
                    let text = derived.text.clone();
                    drop(inner);
                    return Some(text.as_deref().map(str::to_owned));
                }
            }
            (std::sync::Arc::clone(&entry.response.body), entry.stored)
        };

        // Parsing can be expensive and must not hold the cache mutex. Decode
        // exactly as EngineResponse::html does, borrowing valid UTF-8 bytes.
        let html = scraper::Html::parse_document(&String::from_utf8_lossy(&body));
        let text = crate::content::main_text_limited(&html, max_chars);
        let text = if text.is_empty() { None } else { Some(text) };
        self.store_page_text(key, &body, stored, max_chars, text.as_deref());
        Some(text)
    }

    /// Admission is conditional on the raw body still being the generation
    /// parsed above. It never extends the raw TTL or evicts another response.
    fn store_page_text(
        &self,
        key: &str,
        body: &std::sync::Arc<Vec<u8>>,
        stored: Instant,
        max_chars: usize,
        text: Option<&str>,
    ) -> bool {
        let text_bytes = text.map_or(0, str::len);
        if text_bytes > MAX_DERIVED_PAGE_TEXT_BYTES {
            return false;
        }
        let accounted_bytes = text_bytes + DERIVED_PAGE_TEXT_OVERHEAD;
        let mut inner = self.lock();
        let Some(entry) = inner.entries.get(key) else {
            return false;
        };
        if entry.stored != stored
            || entry.stored.elapsed() > self.ttl
            || !std::sync::Arc::ptr_eq(body, &entry.response.body)
        {
            return false;
        }
        let previous_bytes = entry
            .derived_page_text
            .as_ref()
            .map_or(0, |derived| derived.accounted_bytes);
        let Some(next_bytes) = inner
            .bytes
            .checked_sub(previous_bytes)
            .and_then(|bytes| bytes.checked_add(accounted_bytes))
        else {
            return false;
        };
        if next_bytes > self.max_bytes {
            return false;
        }
        let entry = inner.entries.get_mut(key).expect("entry checked above");
        entry.size = entry.size - previous_bytes + accounted_bytes;
        entry.derived_page_text = Some(DerivedPageText {
            max_chars,
            text: text.map(std::sync::Arc::from),
            accounted_bytes,
        });
        inner.bytes = next_bytes;
        inner.derived_bytes = inner.derived_bytes - previous_bytes + accounted_bytes;
        true
    }

    pub fn set(&self, key: &str, response: CachedResponse) {
        let size = response.body.len() + key.len() + 128;
        if size > self.max_bytes {
            self.error.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut inner = self.lock();
        self.remove_locked(&mut inner, key);
        inner.bytes += size;
        let owned_key: Arc<str> = Arc::from(key);
        let previous = inner.newest.clone();
        if let Some(tail) = &previous {
            inner
                .entries
                .get_mut(tail.as_ref())
                .expect("tail exists")
                .next = Some(Arc::clone(&owned_key));
        } else {
            inner.oldest = Some(Arc::clone(&owned_key));
        }
        inner.entries.insert(
            Arc::clone(&owned_key),
            Entry {
                stored: Instant::now(),
                response,
                size,
                derived_page_text: None,
                previous,
                next: None,
            },
        );
        inner.newest = Some(owned_key);
        self.evict_locked(&mut inner);
    }

    fn remove_locked(&self, inner: &mut Inner, key: &str) {
        if let Some(entry) = inner.entries.remove(key) {
            inner.bytes = inner.bytes.saturating_sub(entry.size);
            if let Some(derived) = entry.derived_page_text {
                inner.derived_bytes -= derived.accounted_bytes;
            }
            match &entry.previous {
                Some(previous) => {
                    inner
                        .entries
                        .get_mut(previous.as_ref())
                        .expect("previous exists")
                        .next = entry.next.clone();
                }
                None => inner.oldest = entry.next.clone(),
            }
            match &entry.next {
                Some(next) => {
                    inner
                        .entries
                        .get_mut(next.as_ref())
                        .expect("next exists")
                        .previous = entry.previous;
                }
                None => inner.newest = entry.previous,
            }
        }
    }

    fn evict_locked(&self, inner: &mut Inner) {
        // Preserve the prior derived-first policy AND its insertion-order choice.
        // This occasional derived scan remains O(n); ordinary replace/expire and
        // raw eviction no longer linearly search and shift a VecDeque.
        let mut cursor = inner.oldest.clone();
        while inner.bytes > self.max_bytes && inner.derived_bytes > 0 {
            let Some(key) = cursor else { break };
            let entry = inner
                .entries
                .get_mut(key.as_ref())
                .expect("ordered entry exists");
            cursor = entry.next.clone();
            if let Some(derived) = entry.derived_page_text.take() {
                entry.size -= derived.accounted_bytes;
                inner.bytes -= derived.accounted_bytes;
                inner.derived_bytes -= derived.accounted_bytes;
            }
        }
        while inner.bytes > self.max_bytes {
            let Some(key) = inner.oldest.clone() else {
                break;
            };
            self.remove_locked(inner, key.as_ref());
        }
    }

    /// Drop every entry (used by tests and `/stats`).
    pub fn clear(&self) {
        let mut inner = self.lock();
        // Also release table capacity rather than retaining the high-water mark.
        *inner = Inner::default();
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            hit: self.hit.load(Ordering::Relaxed),
            miss: self.miss.load(Ordering::Relaxed),
            entries: inner.entries.len(),
            bytes: inner.bytes,
            error: self.error.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_order(cache: &ResponseCache, expected: &[String]) {
        let inner = cache.lock();
        let mut cursor = inner.oldest.clone();
        let mut previous: Option<Arc<str>> = None;
        let mut actual = Vec::new();
        while let Some(key) = cursor {
            assert!(actual.len() < inner.entries.len(), "cycle in cache links");
            let entry = &inner.entries[key.as_ref()];
            assert_eq!(entry.previous, previous);
            actual.push(key.to_string());
            previous = Some(key);
            cursor = entry.next.clone();
        }
        assert_eq!(previous, inner.newest);
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), inner.entries.len());
        assert_eq!(
            inner.bytes,
            inner.entries.values().map(|e| e.size).sum::<usize>()
        );
        assert_eq!(
            inner.derived_bytes,
            inner
                .entries
                .values()
                .map(|e| {
                    e.derived_page_text
                        .as_ref()
                        .map_or(0, |d| d.accounted_bytes)
                })
                .sum::<usize>()
        );
    }

    #[test]
    fn linked_order_matches_independent_queue_under_random_churn() {
        let cache = ResponseCache::new(Duration::from_secs(3600), 1 << 24);
        let mut model = std::collections::VecDeque::<String>::new();
        let mut rng = 0x91a5_f123_08c1_u64;
        for step in 0..10_000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let key = format!("key-{}", rng % 257);
            if step % 7 == 0 {
                cache.remove_locked(&mut cache.lock(), &key);
                model.retain(|k| k != &key);
            } else {
                cache.set(&key, response("body"));
                model.retain(|k| k != &key);
                model.push_back(key.clone());
            }
            if step % 13 == 0 {
                let _ = cache.get(&key); // Reads must not move insertion order.
            }
            assert_order(&cache, &model.iter().cloned().collect::<Vec<_>>());
        }
        cache.clear();
        assert_order(&cache, &[]);
    }

    #[test]
    fn linked_order_handles_head_tail_middle_and_singleton() {
        let cache = ResponseCache::default();
        for key in ["a", "b", "c", "d"] {
            cache.set(key, response("x"));
        }
        for (key, expected) in [
            ("b", vec!["a", "c", "d"]),
            ("a", vec!["c", "d"]),
            ("d", vec!["c"]),
            ("c", vec![]),
        ] {
            cache.remove_locked(&mut cache.lock(), key);
            assert_order(
                &cache,
                &expected
                    .iter()
                    .map(|k| (*k).to_string())
                    .collect::<Vec<_>>(),
            );
        }
        cache.set("new", response("x"));
        assert_order(&cache, &["new".to_string()]);
    }

    /// Random churn under a tight budget, with expiry and derived text,
    /// compared against a FIFO model using the same size rule, so eviction
    /// and expiry are exercised (the test above never evicts).
    #[test]
    fn linked_order_matches_queue_model_under_eviction_and_expiry() {
        let ttl = Duration::from_secs(60);
        let max_bytes = 4096;
        let cache = ResponseCache::new(ttl, max_bytes);
        let mut model = std::collections::VecDeque::<(String, usize)>::new();
        let mut rng = 0x1234_5678_9abc_def1_u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut evictions = 0;
        for step in 0..20_000 {
            let key = format!("k{}", next() % 61);
            match next() % 10 {
                0 => {
                    if let Some(entry) = cache.lock().entries.get_mut(key.as_str()) {
                        entry.stored = Instant::now() - ttl - Duration::from_secs(1);
                    }
                    assert!(cache.get(&key).is_none());
                    model.retain(|(k, _)| k != &key);
                }
                1 => {
                    let _ = cache.page_text(&key, 1000);
                }
                _ => {
                    let body_len = (next() % 700) as usize;
                    let body = format!("<article>{}</article>", "x".repeat(body_len));
                    let size = body.len() + key.len() + 128;
                    cache.set(&key, response(&body));
                    model.retain(|(k, _)| k != &key);
                    model.push_back((key.clone(), size));
                    let mut total: usize = model.iter().map(|(_, s)| *s).sum();
                    while total > max_bytes {
                        let (_, s) = model.pop_front().unwrap();
                        total -= s;
                        evictions += 1;
                    }
                }
            }
            let expected: Vec<String> = model.iter().map(|(k, _)| k.clone()).collect();
            assert_order(&cache, &expected);
            assert!(cache.lock().bytes <= max_bytes, "step {step}");
        }
        assert!(
            evictions > 1000,
            "the test must actually evict: {evictions}"
        );
    }

    fn response(body: &str) -> CachedResponse {
        CachedResponse {
            status: 200,
            final_url: "https://example.com/".into(),
            headers: Default::default(),
            body: std::sync::Arc::new(body.as_bytes().to_vec()),
        }
    }

    #[test]
    fn stored_responses_are_returned() {
        let cache = ResponseCache::default();
        cache.set("GET https://example.com/ ", response("hello"));
        let cached = cache.get("GET https://example.com/ ").expect("cached");
        assert_eq!(cached.body.len(), 5);
        assert_eq!(cache.stats().hit, 1);
    }

    #[test]
    fn missing_keys_count_as_misses() {
        let cache = ResponseCache::default();
        assert!(cache.get("nope").is_none());
        assert_eq!(cache.stats().miss, 1);
    }

    #[test]
    fn entries_expire_after_the_ttl() {
        let cache = ResponseCache::new(Duration::from_millis(0), 1024);
        cache.set("key", response("x"));
        std::thread::sleep(Duration::from_millis(2));
        assert!(cache.get("key").is_none());
    }

    #[test]
    fn the_cache_stays_within_its_size_budget() {
        let cache = ResponseCache::new(Duration::from_secs(60), 200);
        for index in 0..50 {
            cache.set(&format!("key-{index}"), response(&"x".repeat(64)));
        }
        assert!(cache.stats().bytes <= 200);
    }

    #[test]
    fn oversized_responses_are_not_cached() {
        let cache = ResponseCache::new(Duration::from_secs(60), 16);
        cache.set("key", response(&"x".repeat(100)));
        assert!(cache.get("key").is_none());
    }

    fn generation(cache: &ResponseCache, key: &str) -> (std::sync::Arc<Vec<u8>>, Instant) {
        let inner = cache.lock();
        let entry = inner.entries.get(key).expect("raw entry");
        (std::sync::Arc::clone(&entry.response.body), entry.stored)
    }

    #[test]
    fn derived_page_text_reuses_one_slot_and_counts_one_hit() {
        let cache = ResponseCache::default();
        cache.set("page", response("<article>Hello   world</article>"));
        let before = cache.stats().bytes;
        let (_, stored) = generation(&cache, "page");
        assert_eq!(
            cache.page_text("page", 8000),
            Some(Some("Hello world".into()))
        );
        let first = cache.lock().entries["page"]
            .derived_page_text
            .as_ref()
            .unwrap()
            .text
            .clone()
            .unwrap();
        assert_eq!(
            cache.page_text("page", 8000),
            Some(Some("Hello world".into()))
        );
        let inner = cache.lock();
        let entry = &inner.entries["page"];
        let second = entry
            .derived_page_text
            .as_ref()
            .unwrap()
            .text
            .as_ref()
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, second));
        assert_eq!(entry.stored, stored, "derivation must not refresh the TTL");
        drop(inner);
        assert_eq!(cache.stats().hit, 2);
        assert_eq!(cache.stats().miss, 0);
        assert_eq!(
            cache.stats().bytes,
            before + 11 + DERIVED_PAGE_TEXT_OVERHEAD
        );
    }

    #[test]
    fn derived_missing_and_empty_are_distinct_without_double_counting_misses() {
        let cache = ResponseCache::default();
        assert_eq!(cache.page_text("missing", 8000), None);
        assert_eq!(cache.stats().miss, 0);
        assert!(cache.get("missing").is_none());
        assert_eq!(cache.stats().miss, 1);
        cache.set("empty", response("<article> \t\n </article>"));
        assert_eq!(cache.page_text("empty", 8000), Some(None));
        assert_eq!(cache.page_text("empty", 8000), Some(None));
        assert!(cache.lock().entries["empty"].derived_page_text.is_some());
        assert_eq!(cache.stats().hit, 2);
        assert_eq!(cache.stats().miss, 1);
    }

    #[test]
    fn different_limits_replace_the_single_derived_variant_and_its_accounting() {
        let cache = ResponseCache::default();
        cache.set("page", response("<article>你好 🦀 café</article>"));
        let before = cache.stats().bytes;
        for (limit, expected) in [
            (2, "你好…"),
            (4, "你好 🦀…"),
            (0, "…"),
            (40, "你好 🦀 café"),
        ] {
            assert_eq!(cache.page_text("page", limit), Some(Some(expected.into())));
            assert_eq!(
                cache.stats().bytes,
                before + expected.len() + DERIVED_PAGE_TEXT_OVERHEAD
            );
            assert_eq!(
                cache.lock().entries["page"]
                    .derived_page_text
                    .as_ref()
                    .unwrap()
                    .max_chars,
                limit
            );
        }
    }

    #[test]
    fn derived_text_uses_the_raw_ttl_and_does_not_cache_after_expiration() {
        let cache = ResponseCache::new(Duration::from_secs(60), 8192);
        cache.set("page", response("<article>old</article>"));
        cache.lock().entries.get_mut("page").unwrap().stored =
            Instant::now() - Duration::from_secs(30);
        let (body, stored) = generation(&cache, "page");
        assert_eq!(cache.page_text("page", 20), Some(Some("old".into())));
        assert_eq!(generation(&cache, "page").1, stored);
        let expired = Instant::now() - Duration::from_secs(61);
        cache.lock().entries.get_mut("page").unwrap().stored = expired;
        assert!(!cache.store_page_text("page", &body, expired, 20, Some("late")));
        assert_eq!(cache.page_text("page", 20), None);
        assert_eq!(cache.stats().entries, 0);
        assert_eq!(cache.stats().bytes, 0);
        assert_eq!(cache.stats().miss, 0);
    }

    #[test]
    fn replacement_and_clear_discard_derived_text_and_reject_in_flight_writes() {
        let cache = ResponseCache::default();
        cache.set("page", response("<article>old</article>"));
        let (old_body, old_stored) = generation(&cache, "page");
        assert_eq!(cache.page_text("page", 20), Some(Some("old".into())));
        cache.set("page", response("<article>new</article>"));
        assert!(!cache.store_page_text("page", &old_body, old_stored, 20, Some("old")));
        assert!(cache.lock().entries["page"].derived_page_text.is_none());
        assert_eq!(cache.page_text("page", 20), Some(Some("new".into())));
        let (body, stored) = generation(&cache, "page");
        cache.clear();
        assert!(!cache.store_page_text("page", &body, stored, 20, Some("late")));
        assert_eq!(cache.page_text("page", 20), None);
        assert_eq!(cache.stats().bytes, 0);
        assert_eq!(cache.lock().derived_bytes, 0);
    }

    #[test]
    fn reusing_the_same_body_arc_still_creates_a_new_generation() {
        let cache = ResponseCache::default();
        let raw = response("<article>same bytes</article>");
        cache.set("page", raw.clone());
        cache.lock().entries.get_mut("page").unwrap().stored =
            Instant::now() - Duration::from_secs(1);
        let (body, stored) = generation(&cache, "page");
        cache.set("page", raw);
        assert!(std::sync::Arc::ptr_eq(&body, &generation(&cache, "page").0));
        assert!(!cache.store_page_text("page", &body, stored, 20, Some("stale")));
        assert_eq!(cache.page_text("page", 20), Some(Some("same bytes".into())));
    }

    #[test]
    fn derived_admission_skips_a_full_cache_without_evicting_raw_entries() {
        let raw = "<article>hello</article>";
        let raw_size = raw.len() + 1 + 128;
        let cache = ResponseCache::new(Duration::from_secs(60), 2 * raw_size);
        cache.set("a", response(raw));
        cache.set("b", response(raw));
        assert_eq!(cache.page_text("a", 20), Some(Some("hello".into())));
        assert!(cache.lock().entries["a"].derived_page_text.is_none());
        assert_eq!(cache.stats().entries, 2);
        assert_eq!(cache.stats().bytes, 2 * raw_size);
    }

    #[test]
    fn oversized_unicode_derivations_are_returned_but_not_retained() {
        let cache = ResponseCache::default();
        let raw = format!("<article>{}</article>", "界".repeat(30_000));
        cache.set("page", response(&raw));
        let before = cache.stats().bytes;
        let text = cache.page_text("page", 22_000).unwrap().unwrap();
        assert_eq!(text.chars().count(), 22_001);
        assert!(text.ends_with('…'));
        assert!(text.len() > MAX_DERIVED_PAGE_TEXT_BYTES);
        assert!(cache.lock().entries["page"].derived_page_text.is_none());
        assert_eq!(cache.stats().bytes, before);
    }

    #[test]
    fn raw_insertions_reclaim_derived_bytes_before_evicting_responses() {
        let raw = "<article>hello</article>";
        let raw_size = raw.len() + 1 + 128;
        let cache = ResponseCache::new(Duration::from_secs(60), 3 * raw_size);
        cache.set("a", response(raw));
        cache.set("b", response(raw));
        assert_eq!(cache.page_text("a", 20), Some(Some("hello".into())));
        assert!(cache.lock().entries["a"].derived_page_text.is_some());
        assert_eq!(cache.lock().derived_bytes, 5 + DERIVED_PAGE_TEXT_OVERHEAD);
        cache.set("c", response(raw));
        assert_eq!(cache.stats().entries, 3);
        assert_eq!(cache.stats().bytes, 3 * raw_size);
        assert!(cache.lock().entries["a"].derived_page_text.is_none());
        assert_eq!(cache.lock().derived_bytes, 0);
        let (body, stored) = generation(&cache, "a");
        cache.set("d", response(raw));
        assert_eq!(cache.page_text("a", 20), None);
        assert!(!cache.store_page_text("a", &body, stored, 20, Some("late")));
        assert_eq!(cache.stats().entries, 3);
    }

    #[test]
    fn derived_text_preserves_lossy_utf8_decoding() {
        let cache = ResponseCache::default();
        let mut raw = response("");
        raw.body = std::sync::Arc::new(b"<article>a\xffb</article>".to_vec());
        cache.set("page", raw);
        assert_eq!(cache.page_text("page", 20), Some(Some("a\u{fffd}b".into())));
    }

    #[test]
    fn valkey_derived_lookup_falls_through_without_connecting_or_counting() {
        let cache = Cache::valkey("redis://127.0.0.1:1/0", Duration::from_secs(60));
        assert_eq!(cache.page_text("page", 20), None);
        assert_eq!(cache.stats().hit, 0);
        assert_eq!(cache.stats().miss, 0);
        assert_eq!(cache.stats().error, 0);
    }
}

/// A cache backend: in-process memory or an external Valkey / Redis server.
pub enum CacheBackend {
    Memory(ResponseCache),
    Valkey(ValkeyCache),
}

/// Cache facade used by the network layer and the HTTP handlers.
pub struct Cache {
    backend: CacheBackend,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            backend: CacheBackend::Memory(ResponseCache::default()),
        }
    }
}

impl Cache {
    pub fn memory(ttl: Duration, max_bytes: usize) -> Self {
        Self {
            backend: CacheBackend::Memory(ResponseCache::new(ttl, max_bytes)),
        }
    }

    /// A Valkey / Redis backed cache (upstream valkey.url). The connection is
    /// opened on first use so the searcher can be built synchronously.
    pub fn valkey(url: &str, ttl: Duration) -> Self {
        Self {
            backend: CacheBackend::Valkey(ValkeyCache::new(url, ttl)),
        }
    }

    pub fn is_valkey(&self) -> bool {
        matches!(self.backend, CacheBackend::Valkey(_))
    }

    pub fn key(method: &str, url: &str, body: &str) -> String {
        ResponseCache::key(method, url, body)
    }

    pub async fn get(&self, key: &str) -> Option<CachedResponse> {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.get(key),
            CacheBackend::Valkey(cache) => cache.get(key).await,
        }
    }

    /// Reuse readable text tied to an in-memory raw-response generation.
    /// Valkey retains its existing fetch/parse behavior and wire format.
    pub fn page_text(&self, key: &str, max_chars: usize) -> Option<Option<String>> {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.page_text(key, max_chars),
            CacheBackend::Valkey(_) => None,
        }
    }

    pub async fn set(&self, key: &str, response: CachedResponse) {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.set(key, response),
            CacheBackend::Valkey(cache) => cache.set(key, response).await,
        }
    }

    pub async fn clear(&self) {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.clear(),
            CacheBackend::Valkey(cache) => cache.clear().await,
        }
    }

    pub fn stats(&self) -> CacheStats {
        match &self.backend {
            CacheBackend::Memory(cache) => cache.stats(),
            CacheBackend::Valkey(cache) => cache.stats(),
        }
    }
}

/// A cache stored in Valkey / Redis, like the upstream valkey cache backend.
pub struct ValkeyCache {
    url: String,
    connection: tokio::sync::OnceCell<tokio::sync::Mutex<ValkeyConnection>>,
    ttl: Duration,
    hit: AtomicU64,
    miss: AtomicU64,
    error: AtomicU64,
    entries: AtomicU64,
}

impl ValkeyCache {
    pub fn new(url: &str, ttl: Duration) -> Self {
        Self {
            url: url.to_string(),
            connection: tokio::sync::OnceCell::new(),
            ttl,
            hit: AtomicU64::new(0),
            miss: AtomicU64::new(0),
            error: AtomicU64::new(0),
            entries: AtomicU64::new(0),
        }
    }

    async fn connection(&self) -> Option<&tokio::sync::Mutex<ValkeyConnection>> {
        self.connection
            .get_or_try_init(|| async {
                ValkeyConnection::connect(&self.url)
                    .await
                    .map(tokio::sync::Mutex::new)
            })
            .await
            .ok()
    }

    pub async fn get(&self, key: &str) -> Option<CachedResponse> {
        let Some(connection) = self.connection().await else {
            self.error.fetch_add(1, Ordering::Relaxed);
            self.miss.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let mut connection = connection.lock().await;
        match connection.command(&["GET", key]).await {
            Ok(Some(payload)) => match decode_response(&payload) {
                Some(response) => {
                    self.hit.fetch_add(1, Ordering::Relaxed);
                    Some(response)
                }
                None => {
                    self.error.fetch_add(1, Ordering::Relaxed);
                    self.miss.fetch_add(1, Ordering::Relaxed);
                    None
                }
            },
            Ok(None) => {
                self.miss.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(_) => {
                self.error.fetch_add(1, Ordering::Relaxed);
                self.miss.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub async fn set(&self, key: &str, response: CachedResponse) {
        let Some(payload) = encode_response(&response) else {
            self.error.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let Some(connection) = self.connection().await else {
            self.error.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let ttl = self.ttl.as_secs().max(1).to_string();
        let mut connection = connection.lock().await;
        match connection
            .command(&["SET", key, &payload, "EX", &ttl])
            .await
        {
            Ok(_) => {
                self.entries.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.error.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub async fn clear(&self) {
        let Some(connection) = self.connection().await else {
            return;
        };
        let mut connection = connection.lock().await;
        if let Ok(Some(keys)) = connection.command(&["KEYS", "searxng:*"]).await {
            for key in keys.split('\n') {
                if !key.is_empty() {
                    let _ = connection.command(&["DEL", key]).await;
                }
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hit: self.hit.load(Ordering::Relaxed),
            miss: self.miss.load(Ordering::Relaxed),
            entries: self.entries.load(Ordering::Relaxed) as usize,
            bytes: 0,
            error: self.error.load(Ordering::Relaxed),
        }
    }
}

/// Encode a cached response as JSON with a base64 body.
fn encode_response(response: &CachedResponse) -> Option<String> {
    use base64::Engine as _;
    let document = serde_json::json!({
        "status": response.status,
        "final_url": response.final_url,
        "headers": response.headers,
        "body": base64::engine::general_purpose::STANDARD.encode(response.body.as_slice()),
    });
    serde_json::to_string(&document).ok()
}

/// Decode the payload written by encode_response.
fn decode_response(payload: &str) -> Option<CachedResponse> {
    use base64::Engine as _;
    let document: serde_json::Value = serde_json::from_str(payload).ok()?;
    let body = document.get("body").and_then(|value| value.as_str())?;
    let body = base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()?;
    let headers = document
        .get("headers")
        .and_then(|value| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(CachedResponse {
        status: document.get("status").and_then(|value| value.as_u64())? as u16,
        final_url: document
            .get("final_url")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        headers,
        body: std::sync::Arc::new(body),
    })
}

/// A tiny RESP client over a Tokio stream.
struct ValkeyConnection {
    reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
}

impl ValkeyConnection {
    async fn connect(url: &str) -> Result<Self, String> {
        let rest = url
            .strip_prefix("redis://")
            .or_else(|| url.strip_prefix("valkey://"))
            .ok_or_else(|| format!("unsupported cache url: {url}"))?;
        let (authority, database) = match rest.split_once('/') {
            Some((authority, database)) => (authority, database.parse::<i64>().unwrap_or(0)),
            None => (rest, 0),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse::<u16>().unwrap_or(6379)),
            None => (authority.to_string(), 6379),
        };
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|error| format!("cache: cannot connect to {host}:{port}: {error}"))?;
        let (reader, writer) = stream.into_split();
        let mut connection = Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
        };
        if database != 0 {
            let database = database.to_string();
            connection.command(&["SELECT", &database]).await?;
        }
        Ok(connection)
    }

    /// Run one command; bulk replies come back as Some(String), nil as None.
    async fn command(&mut self, args: &[&str]) -> Result<Option<String>, String> {
        use tokio::io::AsyncWriteExt as _;
        let mut request = format!("*{}\r\n", args.len());
        for argument in args {
            request.push_str(&format!("${}\r\n{}\r\n", argument.len(), argument));
        }
        self.writer
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        self.writer
            .flush()
            .await
            .map_err(|error| error.to_string())?;
        match self.read_reply().await? {
            ValkeyReply::Bulk(value) => Ok(value),
            ValkeyReply::Simple(value) => Ok(Some(value)),
            ValkeyReply::Integer(value) => Ok(Some(value.to_string())),
            ValkeyReply::Nil => Ok(None),
            ValkeyReply::Array(items) => Ok(Some(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        ValkeyReply::Bulk(Some(value)) | ValkeyReply::Simple(value) => Some(value),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
        }
    }

    fn read_reply(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ValkeyReply, String>> + Send + '_>>
    {
        Box::pin(self.read_reply_inner())
    }

    async fn read_reply_inner(&mut self) -> Result<ValkeyReply, String> {
        use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if line.is_empty() {
            return Err("cache: connection closed".into());
        }
        let marker = line.chars().next().unwrap_or(' ');
        let rest = line[1..].trim_end().to_string();
        Ok(match marker {
            '+' => ValkeyReply::Simple(rest),
            '-' => return Err(format!("cache: {rest}")),
            ':' => ValkeyReply::Integer(rest.parse().unwrap_or(0)),
            '$' => {
                let length: i64 = rest.parse().unwrap_or(-1);
                if length < 0 {
                    ValkeyReply::Nil
                } else {
                    let mut buffer = vec![0u8; length as usize + 2];
                    self.reader
                        .read_exact(&mut buffer)
                        .await
                        .map_err(|error| error.to_string())?;
                    buffer.truncate(length as usize);
                    ValkeyReply::Bulk(Some(String::from_utf8_lossy(&buffer).to_string()))
                }
            }
            '*' => {
                let count: i64 = rest.parse().unwrap_or(0);
                let mut items = Vec::new();
                for _ in 0..count.max(0) {
                    items.push(self.read_reply().await?);
                }
                ValkeyReply::Array(items)
            }
            _ => ValkeyReply::Nil,
        })
    }
}

enum ValkeyReply {
    Simple(String),
    Bulk(Option<String>),
    Integer(i64),
    Array(Vec<ValkeyReply>),
    Nil,
}

#[cfg(test)]
mod valkey_tests {
    use super::*;

    #[test]
    fn responses_round_trip_through_the_valkey_payload() {
        let response = CachedResponse {
            status: 200,
            final_url: "https://example.test/".into(),
            headers: [("content-type".to_string(), "text/html".to_string())]
                .into_iter()
                .collect(),
            body: std::sync::Arc::new(b"hello".to_vec()),
        };
        let payload = encode_response(&response).expect("encode");
        let decoded = decode_response(&payload).expect("decode");
        assert_eq!(decoded.status, 200);
        assert_eq!(decoded.final_url, "https://example.test/");
        assert_eq!(decoded.body.as_slice(), b"hello");
        assert_eq!(
            decoded.headers.get("content-type").map(String::as_str),
            Some("text/html")
        );
    }
}
