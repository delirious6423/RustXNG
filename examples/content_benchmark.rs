// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic paired benchmark of readable-text extraction.
//!
//! Run in release mode and redirect stdout to retain the full JSON evidence:
//! cargo run --release --no-default-features --example content_benchmark --
//!     --rounds 15 --limits 4000,8000,12000 > content-benchmark.json
//!
//! This measures the extraction function, separately with and without DOM
//! parsing. It does not measure HTTP search latency or whole-process RSS.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::time::Instant;

use scraper::Html;
use serde::Serialize;
use sha2::{Digest, Sha256};

const BASELINE_COMMIT: &str = "fef998b69cc1882a9258a7f2cba59bf77793d729";
const DEFAULT_SEED: u64 = 0x5255_5354_584e_4701;

struct MeasuringAllocator;

static TRACK_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
static ALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);
static REALLOC_CALLS: AtomicUsize = AtomicUsize::new(0);

fn record_live_delta(delta: isize) {
    let live = LIVE_BYTES.fetch_add(delta, Ordering::Relaxed) + delta;
    PEAK_BYTES.fetch_max(live.max(0) as usize, Ordering::Relaxed);
}

// SAFETY: All pointers and layouts are forwarded unchanged to System. The
// accounting uses allocation-free atomics and never reads allocated memory.
unsafe impl GlobalAlloc for MeasuringAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc passes the valid allocation layout through.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            record_live_delta(layout.size() as isize);
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: GlobalAlloc passes the valid allocation layout through.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            record_live_delta(layout.size() as isize);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            record_live_delta(-(layout.size() as isize));
        }
        // SAFETY: GlobalAlloc passes the original pointer and allocation layout.
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: GlobalAlloc passes the existing allocation and valid new size.
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() && TRACK_ALLOCATIONS.load(Ordering::Relaxed) {
            REALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(new_size, Ordering::Relaxed);
            record_live_delta(new_size as isize - layout.size() as isize);
        }
        resized
    }
}

#[global_allocator]
static ALLOCATOR: MeasuringAllocator = MeasuringAllocator;

/// Frozen original implementation from BASELINE_COMMIT, src/content.rs.
/// Keep the whole-content collection and truncation intact as the oracle.
fn baseline_main_text_limited(html: &Html, max: usize) -> String {
    const CANDIDATES: &[&str] = &["article", "main", "[role=main]", "body"];
    for selector in CANDIDATES {
        let Ok(selector) = scraper::Selector::parse(selector) else {
            continue;
        };
        if let Some(element) = html.select(&selector).next() {
            let text = baseline_normalise(&element.text().collect::<Vec<_>>().join(" "));
            if !text.is_empty() {
                return baseline_truncate(&text, max);
            }
        }
    }
    String::new()
}

fn baseline_normalise(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn baseline_truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[derive(Clone, Copy)]
enum Variant {
    Baseline,
    Optimized,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Extraction,
    ParseAndExtraction,
}

#[derive(Serialize)]
struct AllocationSample {
    /// Requested sizes summed over alloc/alloc_zeroed/realloc. A realloc adds
    /// its full new requested size, regardless of whether it moved in memory.
    cumulative_requested_bytes: usize,
    /// Maximum sum of simultaneously live requested allocation sizes while
    /// the function executes. This excludes allocator overhead and RSS.
    peak_live_requested_bytes: usize,
    live_requested_bytes_with_returned_string: isize,
    allocation_calls: usize,
    reallocation_calls: usize,
}

#[derive(Serialize)]
struct Pair {
    round: usize,
    optimized_ran_first: bool,
    control_ran_first: bool,
    baseline_us: f64,
    optimized_us: f64,
    control_a_us: f64,
    control_b_us: f64,
}

#[derive(Serialize)]
struct Summary {
    baseline_median_us: f64,
    optimized_median_us: f64,
    median_time_ratio: f64,
    paired_median_time_ratio: f64,
    paired_median_time_ratio_bootstrap_95_percent: [f64; 2],
    aa_control_median_ratio: f64,
    aa_control_median_ratio_bootstrap_95_percent: [f64; 2],
    peak_live_requested_byte_ratio: f64,
    cumulative_requested_byte_ratio: f64,
}

#[derive(Serialize)]
struct Measurement {
    fixture: String,
    fixture_source: String,
    fixture_weight: usize,
    html_bytes: usize,
    html_sha256: String,
    max_chars: usize,
    stage: Stage,
    iterations_per_sample: usize,
    output_bytes: usize,
    output_chars: usize,
    output_sha256: String,
    byte_for_byte_equal: bool,
    baseline_allocations: AllocationSample,
    optimized_allocations: AllocationSample,
    summary: Summary,
    pairs: Vec<Pair>,
}

struct Fixture {
    name: String,
    source: String,
    weight: usize,
    raw: String,
    dom: Html,
}

fn page(name: &'static str, weight: usize, pattern: &str, target_bytes: usize) -> Fixture {
    let mut raw = String::from("<!doctype html><html><body><nav>Menu</nav><article>");
    while raw.len() < target_bytes {
        raw.push_str(pattern);
    }
    raw.push_str("</article><footer>Footer</footer></body></html>");
    let dom = Html::parse_document(&raw);
    Fixture {
        name: name.to_owned(),
        source: "deterministic synthetic HTML".into(),
        weight,
        raw,
        dom,
    }
}

fn fixtures() -> Vec<Fixture> {
    let paragraph = "<p>Rust async tasks share immutable data. This fixed reference paragraph explains scheduling, network requests, allocation and readable page text. </p>\n";
    vec![
        page("short_3k", 40, paragraph, 3 * 1024),
        page("article_32k", 25, paragraph, 32 * 1024),
        page("article_128k", 15, paragraph, 128 * 1024),
        page("reference_512k", 10, paragraph, 512 * 1024),
        page("reference_2m", 5, paragraph, 2 * 1024 * 1024),
        page(
            "nested_128k",
            3,
            "<div><p><span>deep</span><b>nested</b> <em>words</em></p><p> a\t b\n c </p></div>",
            128 * 1024,
        ),
        page(
            "unicode_128k",
            2,
            "<p>台灣\u{3000}網頁\u{2003}搜尋工具 與記憶體\u{a0}效能。 日本語テキスト\t café 🦀🚀 Ελληνικά العربية </p>\n",
            128 * 1024,
        ),
        // A stress case, deliberately excluded from the fixture-weight mix.
        page(
            "single_token_2m_stress",
            0,
            &format!("<p>{}</p>", "x".repeat(2 * 1024 * 1024)),
            2 * 1024 * 1024,
        ),
    ]
}

fn add_snapshot_fixtures(
    fixtures: &mut Vec<Fixture>,
    directory: &std::path::Path,
) -> Result<(), String> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|e| format!("read HTML directory: {e}"))? {
        let entry = entry.map_err(|e| format!("read HTML directory entry: {e}"))?;
        if entry
            .file_type()
            .map_err(|e| format!("inspect HTML entry: {e}"))?
            .is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "html")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err(format!("no regular .html files in {}", directory.display()));
    }
    for path in paths {
        let raw =
            std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let dom = Html::parse_document(&raw);
        fixtures.push(Fixture {
            name: format!(
                "real_{}",
                path.file_stem().unwrap_or_default().to_string_lossy()
            ),
            source: path.to_string_lossy().into_owned(),
            weight: 0,
            raw,
            dom,
        });
    }
    Ok(())
}

fn run(fixture: &Fixture, max: usize, stage: Stage, variant: Variant) -> String {
    let extractor = match variant {
        Variant::Baseline => baseline_main_text_limited,
        Variant::Optimized => rustxng::content::main_text_limited,
    };
    match stage {
        Stage::Extraction => extractor(black_box(&fixture.dom), black_box(max)),
        Stage::ParseAndExtraction => {
            let dom = Html::parse_document(black_box(&fixture.raw));
            extractor(black_box(&dom), black_box(max))
        }
    }
}

fn time(fixture: &Fixture, max: usize, stage: Stage, variant: Variant, iterations: usize) -> f64 {
    let started = Instant::now();
    for _ in 0..iterations {
        // Include disposal of the returned string, equally for both variants.
        drop(black_box(run(fixture, max, stage, variant)));
    }
    started.elapsed().as_secs_f64() * 1_000_000.0 / iterations as f64
}

fn allocations(fixture: &Fixture, max: usize, stage: Stage, variant: Variant) -> AllocationSample {
    // Fixtures and selectors have already been warmed. Their allocations are
    // outside this region; the parsed DOM is also outside Extraction samples.
    LIVE_BYTES.store(0, Ordering::Relaxed);
    PEAK_BYTES.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    REALLOC_CALLS.store(0, Ordering::Relaxed);
    TRACK_ALLOCATIONS.store(true, Ordering::Relaxed);
    let output = black_box(run(fixture, max, stage, variant));
    TRACK_ALLOCATIONS.store(false, Ordering::Relaxed);
    let sample = AllocationSample {
        cumulative_requested_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        peak_live_requested_bytes: PEAK_BYTES.load(Ordering::Relaxed),
        live_requested_bytes_with_returned_string: LIVE_BYTES.load(Ordering::Relaxed),
        allocation_calls: ALLOC_CALLS.load(Ordering::Relaxed),
        reallocation_calls: REALLOC_CALLS.load(Ordering::Relaxed),
    };
    assert!(sample.live_requested_bytes_with_returned_string >= 0);
    drop(output);
    sample
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn boolean(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    if values.len() & 1 == 0 {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn bootstrap_median_interval(values: &[f64], rng: &mut Rng) -> [f64; 2] {
    let mut estimates = Vec::with_capacity(1000);
    for _ in 0..1000 {
        estimates.push(median(
            (0..values.len())
                .map(|_| values[rng.next() as usize % values.len()])
                .collect(),
        ));
    }
    estimates.sort_by(f64::total_cmp);
    [estimates[24], estimates[974]]
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_quality(fixtures: &[Fixture], limits: &[usize]) -> usize {
    let cases = [
        "",
        "<body> \t\n\u{3000}</body>",
        "<article></article><main>Main fallback</main>",
        "<article>  </article><article>Second article is ignored</article><body>Body fallback</body>",
        "<article><span>ab</span><b>cd</b></article>",
        "<main>ab \t\n cd  </main>",
        "<div role=main>角色 主內容</div>",
        "<article>a\u{a0}b\u{2003}c\u{3000}d</article>",
        "<article>🦀 café 日本語 e\u{301}</article>",
        "<body><p>a&amp;b&lt;c&gt;</p><!-- hidden --> <script>script text</script></body>",
        "<article><p>abcdef</p>    </article>",
    ];
    let mut checks = 0;
    let mut compare = |raw: &str, limit: usize| {
        let dom = Html::parse_document(raw);
        let expected = baseline_main_text_limited(&dom, limit);
        let actual = rustxng::content::main_text_limited(&dom, limit);
        assert_eq!(
            expected, actual,
            "output mismatch at limit={limit}, raw={raw:?}"
        );
        checks += 1;
    };
    for raw in cases {
        for limit in [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 50, 4000, 8000] {
            compare(raw, limit);
        }
    }
    let tokens = [
        "hello", "台灣", "a", " ", "\t", "\n", "\u{a0}", "\u{3000}", "🦀", "café", "<span>",
        "</span>", "<p>", "</p>", "&amp;",
    ];
    let mut rng = Rng(DEFAULT_SEED);
    for case in 0..400 {
        let mut raw = String::from("<body><article>");
        for _ in 0..(rng.next() % 180) {
            raw.push_str(tokens[rng.next() as usize % tokens.len()]);
        }
        raw.push_str("</article></body>");
        compare(&raw, case % 97);
    }
    for fixture in fixtures {
        for &limit in limits {
            compare(&fixture.raw, limit);
        }
    }
    checks
}

fn measure(
    fixture: &Fixture,
    max: usize,
    stage: Stage,
    options: &Options,
    rng: &mut Rng,
) -> Measurement {
    let expected = run(fixture, max, stage, Variant::Baseline);
    let actual = run(fixture, max, stage, Variant::Optimized);
    assert_eq!(expected, actual, "fixture={} limit={max}", fixture.name);
    for _ in 0..3 {
        drop(run(fixture, max, stage, Variant::Baseline));
        drop(run(fixture, max, stage, Variant::Optimized));
    }
    let one_us = time(fixture, max, stage, Variant::Baseline, 1).max(0.001);
    let iterations = ((options.target_ms * 1000.0 / one_us).ceil() as usize).clamp(1, 2000);
    let mut pairs = Vec::with_capacity(options.rounds);
    for round in 0..options.rounds {
        let optimized_ran_first = rng.boolean();
        let control_ran_first = rng.boolean();
        let run_ab = || {
            if optimized_ran_first {
                let optimized = time(fixture, max, stage, Variant::Optimized, iterations);
                let baseline = time(fixture, max, stage, Variant::Baseline, iterations);
                (baseline, optimized)
            } else {
                let baseline = time(fixture, max, stage, Variant::Baseline, iterations);
                let optimized = time(fixture, max, stage, Variant::Optimized, iterations);
                (baseline, optimized)
            }
        };
        let mut run_aa = || {
            let first = time(fixture, max, stage, Variant::Baseline, iterations);
            let second = time(fixture, max, stage, Variant::Baseline, iterations);
            if rng.boolean() {
                (first, second)
            } else {
                (second, first)
            }
        };
        let ((baseline_us, optimized_us), (control_a_us, control_b_us)) = if control_ran_first {
            let aa = run_aa();
            (run_ab(), aa)
        } else {
            let ab = run_ab();
            (ab, run_aa())
        };
        pairs.push(Pair {
            round,
            optimized_ran_first,
            control_ran_first,
            baseline_us,
            optimized_us,
            control_a_us,
            control_b_us,
        });
    }
    let baseline_allocations = allocations(fixture, max, stage, Variant::Baseline);
    let optimized_allocations = allocations(fixture, max, stage, Variant::Optimized);
    let old_median = median(pairs.iter().map(|p| p.baseline_us).collect());
    let new_median = median(pairs.iter().map(|p| p.optimized_us).collect());
    let ratios: Vec<_> = pairs
        .iter()
        .map(|p| p.baseline_us / p.optimized_us)
        .collect();
    let aa_ratios: Vec<_> = pairs
        .iter()
        .map(|p| p.control_a_us / p.control_b_us)
        .collect();
    let summary = Summary {
        baseline_median_us: old_median,
        optimized_median_us: new_median,
        median_time_ratio: old_median / new_median,
        paired_median_time_ratio: median(ratios.clone()),
        paired_median_time_ratio_bootstrap_95_percent: bootstrap_median_interval(&ratios, rng),
        aa_control_median_ratio: median(aa_ratios.clone()),
        aa_control_median_ratio_bootstrap_95_percent: bootstrap_median_interval(&aa_ratios, rng),
        peak_live_requested_byte_ratio: baseline_allocations.peak_live_requested_bytes as f64
            / optimized_allocations.peak_live_requested_bytes.max(1) as f64,
        cumulative_requested_byte_ratio: baseline_allocations.cumulative_requested_bytes as f64
            / optimized_allocations.cumulative_requested_bytes.max(1) as f64,
    };
    eprintln!(
        "{} limit={} stage={} time={:.2}x peak-allocated={:.2}x A/A={:.3}",
        fixture.name,
        max,
        match stage {
            Stage::Extraction => "extraction",
            Stage::ParseAndExtraction => "parse+extraction",
        },
        summary.median_time_ratio,
        summary.peak_live_requested_byte_ratio,
        summary.aa_control_median_ratio
    );
    Measurement {
        fixture: fixture.name.to_owned(),
        fixture_source: fixture.source.clone(),
        fixture_weight: fixture.weight,
        html_bytes: fixture.raw.len(),
        html_sha256: fingerprint(fixture.raw.as_bytes()),
        max_chars: max,
        stage,
        iterations_per_sample: iterations,
        output_bytes: actual.len(),
        output_chars: actual.chars().count(),
        output_sha256: fingerprint(actual.as_bytes()),
        byte_for_byte_equal: true,
        baseline_allocations,
        optimized_allocations,
        summary,
        pairs,
    }
}

struct Options {
    rounds: usize,
    limits: Vec<usize>,
    target_ms: f64,
    seed: u64,
    html_dir: Option<PathBuf>,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Self {
            rounds: 15,
            limits: vec![4000, 8000, 12000],
            target_ms: 5.0,
            seed: DEFAULT_SEED,
            html_dir: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            match flag.as_str() {
                "--rounds" => options.rounds = value.parse().map_err(|_| "invalid rounds")?,
                "--limits" => {
                    options.limits = value
                        .split(',')
                        .map(|v| v.parse().map_err(|_| "invalid limit"))
                        .collect::<Result<_, _>>()?
                }
                "--target-ms" => {
                    options.target_ms = value.parse().map_err(|_| "invalid target-ms")?
                }
                "--seed" => options.seed = value.parse().map_err(|_| "invalid seed")?,
                "--html-dir" => options.html_dir = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown flag {flag}")),
            }
        }
        if options.rounds < 3
            || options.limits.is_empty()
            || !options.target_ms.is_finite()
            || options.target_ms <= 0.0
            || options.seed == 0
        {
            return Err(
                "need rounds>=3, nonempty limits, positive finite target-ms, and nonzero seed"
                    .into(),
            );
        }
        Ok(options)
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() -> Result<(), String> {
    let options = Options::parse()?;
    let mut fixtures = fixtures();
    if let Some(directory) = &options.html_dir {
        add_snapshot_fixtures(&mut fixtures, directory)?;
    }
    let quality_checks = verify_quality(&fixtures, &options.limits);
    // Check executable behavior as well as source metadata. A stale baseline
    // library can otherwise be linked when sharing build outputs across two
    // worktrees with the same package name/version.
    let guard_fixture = fixtures
        .iter()
        .find(|fixture| fixture.name == "reference_2m")
        .expect("fixed allocation guard fixture");
    let guard_baseline = allocations(guard_fixture, 8_000, Stage::Extraction, Variant::Baseline);
    let guard_optimized = allocations(guard_fixture, 8_000, Stage::Extraction, Variant::Optimized);
    assert!(guard_baseline.peak_live_requested_bytes > 2 * 1024 * 1024);
    assert!(
        guard_optimized.peak_live_requested_bytes < 128 * 1024,
        "candidate is not bounded: build baseline and candidate in separate target directories"
    );
    let mut rng = Rng(options.seed);
    let started = Instant::now();
    let mut measurements = Vec::new();
    for fixture in &fixtures {
        for &limit in &options.limits {
            for stage in [Stage::Extraction, Stage::ParseAndExtraction] {
                measurements.push(measure(fixture, limit, stage, &options, &mut rng));
            }
        }
    }
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|line| line.starts_with("model name"))
                .map(str::to_owned)
        });
    let output = serde_json::json!({
        "schema_version": 1,
        "baseline_commit": BASELINE_COMMIT,
        "repository_head": command_output("git", &["rev-parse", "HEAD"]),
        "compiler": command_output("rustc", &["--version"]),
        "cpu": cpu,
        "os": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "available_parallelism": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "optimized_source_sha256": fingerprint(include_bytes!("../src/content.rs")),
        "seed": options.seed,
        "rounds": options.rounds,
        "target_ms_per_baseline_sample": options.target_ms,
        "limits": options.limits,
        "html_snapshot_directory": options.html_dir,
        "quality_checks_passed": quality_checks,
        "executable_allocation_guard": {
            "fixture": "reference_2m", "max_chars": 8000,
            "baseline_peak_requested_bytes": guard_baseline.peak_live_requested_bytes,
            "candidate_peak_requested_bytes": guard_optimized.peak_live_requested_bytes,
            "candidate_limit_bytes": 128 * 1024,
        },
        "wall_seconds": started.elapsed().as_secs_f64(),
        "method": {
            "timing": "Single-threaded release binary; paired randomized A/B order plus randomized A/A identity controls; allocation counters disabled during timing, but both variants retain the same allocator tracking-flag branch. Returned-string destruction is included. Three warmup calls per variant and fixed iterations per pair.",
            "allocation": "Separate warmed call, tracking GlobalAlloc requests. Peak live requested bytes exclude allocator metadata, fragmentation, transient internal realloc copies, pre-existing HTML input, and pre-parsed DOM in extraction stage. Parse+extraction includes DOM allocations. Realloc cumulative bytes count the full new requested size, peak live adjusts by size delta. Not RSS or total service memory.",
            "quality": "Byte-for-byte full-output equality against frozen upstream implementation, boundary/Unicode/fallback cases, 400 deterministic generated documents and all fixture/limit combinations; no ranking or network semantics modified.",
            "confidence_interval": "1000 deterministic paired bootstrap resamples of per-round ratios; percentile interval for the median. It captures run-to-run timing variation here, not machine/population variation.",
            "fixture_weights": "Declared synthetic mixture weights sum to 100; not a claim about production traffic. single_token_2m_stress and real HTML snapshots have weight 0 and are reported separately.",
            "scope": "No live requests, no RSS measurement, no search-throughput claim. Do not equate extraction speedup with whole-service speedup."
        },
        "measurements": measurements,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output).map_err(|e| e.to_string())?
    );
    Ok(())
}
