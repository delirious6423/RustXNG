# Reproducing the performance change

Baseline: upstream commit `fef998b69cc1882a9258a7f2cba59bf77793d729` (RustXNG 0.26.0).
Measurements were made on Linux x86_64 with Rust 1.99.1, release optimization,
and the documented `--no-default-features` build for **both** variants. The full
default-feature tests and strict Clippy checks were also run.

## Apply and validate

Apply the supplied patch to the pinned upstream checkout. The patch changes
bounded text extraction and adds opportunistic derived text to the in-memory
response cache. No settings migration is required. The external Valkey backend
continues to use its existing path.

```bash
git clone https://github.com/sahenjp/RustXNG.git
cd RustXNG
git checkout fef998b69cc1882a9258a7f2cba59bf77793d729
git switch -c perf/bounded-grounding
git apply /absolute/path/to/RustXNG-performance.patch
cargo fmt --all --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked --no-default-features --bin rustxng
```

Use `cargo build --release --locked --bin rustxng` instead if database engines
are needed. The tested implementation works with either feature configuration.

## Component benchmark

The example embeds the exact original extraction implementation as its oracle.
It measures extraction separately from HTML parsing plus extraction, checks
exact output bytes, records allocation requests separately from timing, and
includes randomized paired A/B and A/A identity controls.

```bash
cargo run --release --locked --no-default-features \
  --example content_benchmark -- \
  --rounds 15 --limits 4000,8000,12000 > content-benchmark.json
```

Optional real-page inputs:

```bash
python3 tools/fetch_content_benchmark_pages.py /tmp/rustxng-real-pages
cargo run --release --locked --no-default-features \
  --example content_benchmark -- \
  --html-dir /tmp/rustxng-real-pages > content-real-benchmark.json
```

The download helper records exact byte counts and SHA-256 identities. Public
documentation changes over time; compare these identities with
`bench/performance-results/real-page-sources.json` before calling it the same
real-page workload. Synthetic fixtures are generated deterministically and do
not require network access.

## Complete cached API benchmark

Create an original worktree and copy the identical benchmark harness into it.
**Use different Cargo target directories.** A shared target directory can reuse
artifacts from a checkout with the same package name/version and invalidate the
comparison.

```bash
git worktree add --detach ../RustXNG-original \
  fef998b69cc1882a9258a7f2cba59bf77793d729
cp examples/cached_search_benchmark.rs \
  ../RustXNG-original/examples/cached_search_benchmark.rs
CARGO_TARGET_DIR="$PWD/target-candidate" cargo build --release --locked \
  --no-default-features --example cached_search_benchmark
cd ../RustXNG-original
CARGO_TARGET_DIR="$PWD/target-original" cargo build --release --locked \
  --no-default-features --example cached_search_benchmark
cd ../RustXNG
python3 tools/run_cached_search_benchmark.py \
  --baseline ../RustXNG-original/target-original/release/examples/cached_search_benchmark \
  --candidate target-candidate/release/examples/cached_search_benchmark \
  --rounds 8 --iterations 30 \
  --html-dir /tmp/rustxng-real-pages \
  --transport http \
  --output cached-http-benchmark.json
```

The last command requires Linux for its RSS/high-water summary. Omit
`--html-dir` to run only the two deterministic synthetic workloads.

The harness uses the real Axum router and Searcher, engine parsing and ranking,
page-text processing, JSON serialization, and response collection. It starts
with identical raw-response caches and checks the complete response bytes and
cache-hit counters on every request. HTTP mode uses a real loopback TCP listener
and a reused Reqwest client, timing request send through full response receipt.
Use `--transport in-process` to measure just the backend without socket I/O.
It includes three warmup requests per
fresh process. The runner randomizes old/new process order and includes
old/old identity controls. Builds must finish before timed runs start.

The real workload uses five distinct result URLs backed by three saved
documentation snapshots, in this order: Rust ownership, Python asyncio event
loop, SQLite SELECT, Python asyncio event loop, Rust ownership. The repeated
snapshots are explicit and identical in both variants. Expected content is
computed with the original extraction implementation during untimed setup.

## What these measurements mean

- HTTP timings include loopback TCP/HTTP; in-process timings exclude it. Both
  exclude TLS and live upstream latency. They are a
  single-caller, single-engine repeated-grounding workload, not a production
  throughput or live-web-search guarantee.
- Cold requests still need upstream responses and HTML parsing. A cold request
  follows the existing fetch path; a raw-cache hit then computes and memoizes
  text; later hits can reuse that memo while the same raw entry is fresh.
- Derived text uses the existing byte budget and TTL, with one exact-character-
  limit variant per raw body. It is discarded before raw responses under
  pressure. Admission can be skipped, and outputs above 64 KiB are not retained.
- Peak requested allocation bytes in the component benchmark are not process
  RSS. They exclude allocator bookkeeping and pre-existing input/DOM storage.
- Process RSS/high-water values include setup and allocator retention. Real-
  page setup also includes the original extraction oracle and cannot isolate
  request memory usage.
- The declared synthetic fixture weights are not a measured production traffic
  distribution. Aggregate time is `sum(weight * old_time) / sum(weight * new_time)`.
- Read the complete investigation and raw JSON evidence for confidence
  intervals, no-effect controls, output hashes, and scope qualifications.
