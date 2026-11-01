// SPDX-License-Identifier: AGPL-3.0-or-later
//! Copy to examples/cache_churn_benchmark.rs in each build. No new dependencies.
//! Measure the real ResponseCache; this is NOT a model of it.
use rustxng::cache::{CachedResponse, ResponseCache};
use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let entries: usize = args.next().expect("entry count").parse().expect("integer");
    let operations: usize = args
        .next()
        .unwrap_or_else(|| "10000".into())
        .parse()
        .expect("integer");
    assert!(entries > 0 && entries <= 100_000 && operations > 0);
    let cache = ResponseCache::new(Duration::from_secs(3600), 128 * 1024 * 1024);
    let response = CachedResponse {
        status: 200,
        final_url: "https://example.org/".into(),
        headers: Default::default(),
        body: Arc::new(vec![42; 64]),
    };
    let keys: Vec<String> = (0..entries)
        .map(|i| {
            format!(
                "GET https://example.org/search?query=fixed-realistic-length-search-key-{i:08} "
            )
        })
        .collect();
    for key in &keys {
        cache.set(key, response.clone());
    }
    assert_eq!(cache.stats().entries, entries);
    let mut state = 0x81a5c391_u64;
    let schedule: Vec<usize> = (0..operations)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize % entries
        })
        .collect();
    let start = Instant::now();
    for &index in &schedule {
        cache.set(black_box(&keys[index]), response.clone());
    }
    let ns = start.elapsed().as_nanos();
    // Verify data retention, not merely the counter or an optimized-away result.
    for key in &keys {
        assert_eq!(cache.get(key).expect("retained").body.as_slice(), &[42; 64]);
    }
    println!(
        "{}",
        serde_json::json!({"entries": entries, "operations": operations,
        "ns_total": ns, "ns_per_operation": ns as f64 / operations as f64,
        "output_verified": true, "cache_bytes": cache.stats().bytes})
    );
}
