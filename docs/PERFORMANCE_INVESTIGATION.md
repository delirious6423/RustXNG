# RustXNG performance implementation and measurements

Date: 2026-11-02, Asia/Taipei. Upstream baseline: `fef998b69cc1882a9258a7f2cba59bf77793d729` (0.26.0). Local branch: `perf/bounded-content-extraction`.

## Result

The implemented patch exceeds the requested **100x median request-speed target on two warmed HTTP workloads**: 122.413x for a fixed mixed-size page bundle, and 197.671x for saved documentation HTML. The smaller-page workload improves 7.049x. These are real loopback HTTP requests through RustXNG, with identical inputs and complete response bytes.

The working stretch target was **200x**. The best HTTP paired point estimate, **197.671x**, is below that target; it is not rounded up to 200x. The 95% interval is 188.05–212.85x. The backend-only documentation replay reaches 358.606x, but excludes socket transport and is reported separately.

The result is specific to **steady-state cached grounding** with one configured search engine and five cached result pages. It does not establish the same factor for new internet searches, the default multi-engine mix, remote clients, or arbitrary traffic. This is a measured latency improvement, not a maximum-throughput or concurrent-load benchmark.

**Whole-process memory did not fall 100x.** On the mixed HTTP workload, process high-water memory fell from 35,062 to 25,856 KiB, approximately 26.3%. The extraction function separately uses 1,066.11x less peak requested allocation on the 2 MiB text fixture; that is a component allocation result, not total RAM.

## What changed

### Bounded extraction in `src/content.rs`

Previously, extraction collected every text node, joined the complete page, created a whitespace-token vector, joined another complete string, counted all characters, and copied the requested prefix. The service ordinarily retains at most 8,000 characters per page; the standalone helper defaults to 4,000.

The new implementation normalizes while visiting characters and returns after finding the first omitted normalized character. It stores only the retained prefix, preserves inserted spaces between text nodes, and parses built-in CSS selectors once. It preserves Unicode whitespace, scalar-character counting, exact ellipsis behavior, first-element selection, candidate precedence, and empty-container fallbacks.

A long whitespace-only tail can still require scanning to determine whether an ellipsis is needed. Container discovery and HTML DOM construction can still visit the complete document. Those costs are included in the broader measurements.

### Entry-owned derived text in `src/cache.rs` and `src/aggregator.rs`

The raw HTTP response cache already avoided repeat downloads but still copied, parsed, and extracted cached HTML on every grounding request. The patch keeps one derived text variant on the raw cache entry, keyed by its exact character limit. Later requests reuse it while the same raw response remains valid. This also helps different queries that reuse the same page URL.

The cache implementation maintains these invariants:

- Derived text has exactly the raw entry's lifetime. It never refreshes or extends the original TTL.
- Parsing runs outside the cache mutex. Installation checks the same body `Arc`, stored timestamp, and freshness, rejecting stale in-flight writes after replacement, expiry, clearing, or eviction.
- Cached empty content is distinguished from a cache miss.
- One character-limit variant is retained per raw entry. Outputs over 64 KiB are returned correctly but are not retained.
- Derived text and conservative metadata are charged against the existing cache budget. Admission can be skipped when space is unavailable.
- Under pressure, derived text is discarded before raw responses, preserving raw-body retention under the same budget. A private derived-byte total avoids scanning for memos when none exist.
- The public-URL guard executes before the new cache path. Seeded cache entries cannot bypass it.
- The external Valkey backend and serialization retain their existing behavior. Public response-body types are unchanged.

No engine-selection, ranking, relevance, result-limit, or timeout changes were used to obtain the measured improvement.

On a genuinely cold URL, the original fetch path still extracts the page. The next raw-cache access installs the derived memo, and subsequent accesses reuse it. The large steady-state factor therefore does **not** automatically apply to the first repeat. These benchmarks explicitly warm both variants with three requests before measurement.

## Primary measurements: actual HTTP

The timer covers request send through receipt of the complete response body using a reused Reqwest client and an actual Axum listener on `127.0.0.1`. It includes engine-response parsing, ranking, page handling, JSON serialization, TCP/HTTP transport, and response collection. It excludes TLS, upstream internet requests, fixture setup, validation, and digest calculation.

| Workload | Original median (ms) | Modified median (ms) | Paired median speedup | 95% paired bootstrap interval |
| --- | ---: | ---: | ---: | ---: |
| Five 32 KiB pages | 1.429408 | 0.199111 | 7.049x | 6.56–7.68x |
| 4 KiB / 32 KiB / 128 KiB / 512 KiB / 2 MiB | 23.255624 | 0.188440 | 122.413x | 117.79–133.23x |
| Saved documentation replay (five URLs, three snapshots) | 39.843382 | 0.205087 | 197.671x | 188.05–212.85x |

Each latency column is the median of eight per-process medians. The speedup is the median of the eight paired old/new ratios; these summaries need not divide to exactly the same number. The full raw samples (about 1 MB of JSON per run) are kept out of the repository; regenerate them with the commands in `PERFORMANCE_REPRODUCTION.md`.

The real workload uses Rust's ownership chapter, Python's asyncio event-loop documentation, and SQLite's SELECT documentation, in the declared order Rust/Python/SQLite/Python/Rust, under five distinct result URLs. The repeated snapshots are explicit and identical in both variants. This is replay of real HTML, not a live relevance evaluation of those search results. Input URLs, byte lengths, and SHA-256 digests are in `real-page-sources.json`.

### Tail latency and no-effect controls

| Workload | Original request p95 (ms) | Modified request p95 (ms) | A/A median ratio | A/A 95% interval |
| --- | ---: | ---: | ---: | ---: |
| standard | 1.887252 | 0.357521 | 0.9367 | 0.886–1.029 |
| mixed | 30.047132 | 0.382388 | 0.9944 | 0.955–1.087 |
| real | 44.807208 | 0.395097 | 0.9830 | 0.932–1.071 |

A/A runs execute the unchanged original twice, using the same process/warmup structure. Their intervals include 1.0, providing a no-effect baseline. The 100x headline refers to **median** latency; the mixed workload's p95 improvement is smaller. Intervals describe this host and these runs, not variation across all hardware or deployments.

### Process memory

| Workload | Median RSS, original → modified (KiB) | Median high-water, original → modified (KiB) | High-water decrease |
| --- | ---: | ---: | ---: |
| standard | 12,456 → 12,384 | 12,456 → 12,384 | 0.58% |
| mixed | 17,142 → 15,130 | 35,062 → 25,856 | 26.26% |
| real | 38,642 → 30,452 | 38,642 → 30,452 | 21.19% |

These are Linux process readings for the benchmark, including application, client, fixtures, cache, and allocator retention. Real-page setup additionally runs the original extraction oracle, so its high-water value is not an isolated request-memory measurement. The tiny standard-workload difference should be treated as effectively unchanged.

## Component evidence and bottleneck diagnosis

The first implementation removed full-page temporary materialization. At the production limit of 8,000 characters, it measured:

| Input | Extraction speedup, ratio of medians | Peak live requested extraction bytes, old → new | Allocation reduction | Parse + extraction speedup |
| --- | ---: | ---: | ---: | ---: |
| short_3k | 0.994x | 15,098 → 4,096 | 3.69x | 1.008x |
| article_32k | 3.783x | 202,182 → 8,192 | 24.68x | 1.379x |
| article_128k | 13.324x | 545,816 → 8,192 | 66.63x | 1.543x |
| reference_512k | 51.208x | 2,183,369 → 8,192 | 266.52x | 1.616x |
| reference_2m | 193.479x | 8,733,581 → 8,192 | 1066.11x | 1.626x |
| real_rust-ownership | 3.437x | 198,156 → 8,192 | 24.19x | 1.113x |
| real_python-asyncio | 2.571x | 518,919 → 8,192 | 63.34x | 1.014x |
| real_sqlite-select | 1.501x | 928,297 → 8,192 | 113.32x | 0.998x |

The 2 MiB text fixture's extraction falls from 4,023.590 to 20.796 microseconds, with 8,733,581 → 8,192 peak requested bytes. HTML parsing plus extraction improves only 1.626x. The same distinction is especially clear on actual SQLite HTML: extraction improves 1.501x, but parsing plus extraction is effectively unchanged at 0.998x, with a paired interval spanning approximately 1.0.

The declared weighted synthetic mixture improves 24.031x for extraction and 1.475x for parsing plus extraction at 8,000 characters. Its weights are a fixed experiment, not a measured traffic distribution. The 2 MiB single-token stress case has zero weight and is not used as the headline workload.

This diagnosis explained why streaming alone improved complete cached backend requests only about 1.5–1.7x. Memoizing text removes the remaining repeated DOM work and creates the larger whole-request gain.

Requested-allocation accounting is separate from timing. It counts allocator-requested live sizes, excludes allocator metadata/fragmentation, and does not count transient internal copies performed by `realloc`. Existing input and the pre-parsed DOM are outside the extraction-only measurement. Both timed variants use the same allocator wrapper with tracking counters disabled.

### Backend-only results after memoization

| Workload | Original median (ms) | Modified median (ms) | Paired speedup |
| --- | ---: | ---: | ---: |
| standard | 1.245229 | 0.111732 | 10.732x |
| mixed | 22.733608 | 0.107863 | 210.484x |
| real | 42.620266 | 0.115753 | 358.606x |

These exclude TCP/HTTP transport and are not substituted for the primary HTTP result. The primary implementation claim is tied to the HTTP table above.

## Method and verification

Environment: AMD EPYC 9V74 virtual host, Linux x86_64, nine available vCPUs; Rust 1.99.1. The benchmark is a single caller using Tokio's current-thread runtime. Both timed builds use the same release flags, lockfile, and `--no-default-features` configuration. Database engines are disabled in both measurements; full default-feature correctness checks were run separately.

Both cached API variants use one Mwmbl engine, five page bodies, an 8,000-character limit, a 32 MiB memory-cache budget, and the same 3,600-second benchmark TTL. The synthetic workloads are byte-identical before and after both optimization phases. The benchmark checks exactly six cache hits per request, no upstream misses/errors, ordered URLs, titles, snippets, full raw text, and complete JSON response hashes.

The runner uses eight fresh-process A/B pairs per workload, randomizes variant order and control-block order, and measures 30 requests per process after three warmups. A/A controls use the identical original binary twice. Bootstrap intervals resample process pairs rather than treating correlated requests within one process as independent experiments.

Verification completed:

- `cargo test --locked --offline --all-targets`: **813 unit tests + 17 integration tests passed, zero failures**.
- `cargo clippy --locked --offline --all-targets -- -D warnings`: passed.
- `cargo fmt --all --check` and `git diff --check`: passed.
- Component benchmark: **576 differential checks** and all **66** fixture/limit/stage output comparisons passed.
- Cache tests cover exact limits, Unicode, empty/missing distinctions, expiry without TTL extension, stale generation races, replacement, clearing, eviction, budget pressure, oversized output, lossy UTF-8, and Valkey fallback.
- The new URL-guard regression seeds private IPv4/IPv6 and metadata-address content, then proves the normal page-text path rejects it without cache access.
- An independent reviewer inspected the implementation, checked raw samples and executable hashes, recomputed ratios, and verified the final test evidence.

One early component attempt reused a stale original library through a shared Cargo target directory. That run was excluded before publishing a performance claim. Original and candidate now use separate target directories, executable hashes are recorded, and the component harness asserts a large-fixture allocation bound that the original implementation cannot satisfy.

The upstream live-network example was attempted with one public engine and two requests. It returned zero results and an engine error from this environment. It is preserved as a failed connectivity check and contributes no speed or relevance claim. We did not substitute a successful-looking empty response for a valid benchmark.

## Targets and remaining limits

| Goal | Result |
| --- | --- |
| Requested 100x median warm HTTP improvement | Achieved on mixed and documentation workloads; 95% lower bounds exceed 100x. |
| Working 200x HTTP stretch target | Not reached by paired point estimates: best 197.671x, with uncertainty spanning 200x. |
| Small-page 100x improvement | Not reached: 7.049x over HTTP. |
| 100x total process-memory reduction | Not reached: mixed high-water decreases about 26.3%. |
| 100x fresh internet-search improvement | Not established; upstream search and page-fetch latency remain. |
| Default multi-engine production performance | Not established; cached engine parsing/ranking and cache pressure can change the fraction of work saved. |

The measured warm HTTP floor is roughly 0.19–0.21 ms for these outputs. Remaining work includes request handling, engine JSON parsing/ranking, output serialization/copying, and client/server transport. The backend-only data show that eliminating page parsing does not eliminate these costs. Further instruction tuning would need a new profile and a new controlled benchmark; it would not remove internet latency.

For production, the useful next measurement is a representative replay of the actual engine mix, concurrency, cold/warm hit rates, and page sizes. The derived cache can be bypassed by insufficient budget, eviction pressure, external Valkey storage, or oversized derived output. Improving those cases would require a separate bounded design and measurement rather than assuming the current factors transfer.

## Reproduce and inspect

See `docs/PERFORMANCE_REPRODUCTION.md` for complete commands. The package includes the applicable patch, changed source, standalone benchmark examples, download/runner helpers, raw JSON, test/Clippy output, the source/binary identities, and upstream license/attribution files. No dependencies were added and no configuration migration is needed.

Primary upstream source references:

- [Original content extractor](https://github.com/sahenjp/RustXNG/blob/fef998b69cc1882a9258a7f2cba59bf77793d729/src/content.rs)
- [Original raw response cache](https://github.com/sahenjp/RustXNG/blob/fef998b69cc1882a9258a7f2cba59bf77793d729/src/cache.rs)
- [Original aggregation and page-fetch path](https://github.com/sahenjp/RustXNG/blob/fef998b69cc1882a9258a7f2cba59bf77793d729/src/aggregator.rs)
- [Original project benchmark documentation](https://github.com/sahenjp/RustXNG/blob/fef998b69cc1882a9258a7f2cba59bf77793d729/docs/BENCHMARK.md)
