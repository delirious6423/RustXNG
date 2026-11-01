// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic, fully cached search through the real Axum router and Searcher.
//!
//! Run identical copies on the baseline and candidate:
//! cargo run --release --no-default-features --example cached_search_benchmark --
//!     --iterations 30 --workload standard
//! For saved web pages, use --workload real --html-dir /path/to/fixtures.
//! Add --transport http to include HTTP over loopback with a reused client.
//!
//! This includes routing, engine parsing, ranking, raw-page HTML parsing and
//! extraction, JSON serialization, and response-body collection. The default
//! in-process transport excludes TCP; HTTP transport includes loopback TCP and
//! HTTP client/server work. Both exclude TLS and all live upstream I/O. Setup,
//! warmup, validation, hashing, and RSS sampling are outside each timed request.
//! Process RSS/high-water measurements include setup. In the real workload,
//! this includes the frozen reference parser/extractor used as the oracle.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use rustxng::aggregator::Searcher;
use rustxng::cache::{Cache, CachedResponse};
use rustxng::query::Query;
use rustxng::settings::{EngineConfig, LimiterSettings, Settings};
use rustxng::web::{AppState, router};

const QUERY: &str = "rust programming";
const URI: &str = "/search?q=rust+programming&format=json&include_raw_content=1";
const RAW_CHARS: usize = 8_000;
const WARMUP: usize = 3;
const REAL_FILES: [&str; 5] = [
    "rust-ownership.html",
    "python-asyncio.html",
    "sqlite-select.html",
    "python-asyncio.html",
    "rust-ownership.html",
];

struct Expected {
    url: String,
    title: String,
    snippet: String,
    raw: String,
}

enum Transport {
    InProcess(Router),
    Http {
        client: reqwest::Client,
        url: String,
    },
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Construct exactly `bytes` bytes of valid HTML, with known normalized text.
/// The expected prefix is derived from the fixture construction, without using
/// the content extractor under test or constructing a second full-page string.
fn page(index: usize, bytes: usize) -> (String, String) {
    let prefix = "<html><body><article>";
    let suffix = "</article></body></html>";
    let sentence = format!(
        "Rust programming guide {index}: ownership, lifetimes, asynchronous tasks, \
         reliable APIs, Unicode 中文, and memory safety."
    );
    let paragraph = format!("<p>{sentence}</p>");
    let remaining = bytes - prefix.len() - suffix.len();
    let paragraphs = remaining / paragraph.len();
    let tail = "x".repeat(remaining % paragraph.len());
    let mut html = String::with_capacity(bytes);
    html.push_str(prefix);
    let mut expected = String::new();
    let mut expected_chars = 0;
    let mut append_expected = |text: &str| {
        if text.is_empty() || expected_chars > RAW_CHARS {
            return;
        }
        if expected_chars > 0 {
            expected.push(' ');
            expected_chars += 1;
        }
        for character in text.chars().take(RAW_CHARS + 1 - expected_chars) {
            expected.push(character);
            expected_chars += 1;
        }
    };
    for _ in 0..paragraphs {
        html.push_str(&paragraph);
        append_expected(&sentence);
    }
    html.push_str(&tail);
    append_expected(&tail);
    html.push_str(suffix);
    assert_eq!(html.len(), bytes);
    if expected_chars > RAW_CHARS {
        expected.pop();
        expected.push('…');
    }
    (html, expected)
}

/// Frozen upstream extraction, independent of the production implementation.
/// Called only during real-workload setup, outside all request timings.
fn reference_text(html: &scraper::Html, max: usize) -> String {
    for selector in ["article", "main", "[role=main]", "body"] {
        let Ok(selector) = scraper::Selector::parse(selector) else {
            continue;
        };
        if let Some(element) = html.select(&selector).next() {
            let joined = element.text().collect::<Vec<_>>().join(" ");
            let text = joined.split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                if text.chars().count() <= max {
                    return text.to_string();
                }
                let mut out: String = text.chars().take(max).collect();
                out.push('…');
                return out;
            }
        }
    }
    String::new()
}

async fn seed(cache: &Cache, url: &str, body: Vec<u8>, content_type: &str) {
    cache
        .set(
            &Cache::key("GET", url, ""),
            CachedResponse {
                status: 200,
                final_url: url.to_string(),
                headers: BTreeMap::from([("content-type".into(), content_type.into())]),
                body: Arc::new(body),
            },
        )
        .await;
}

fn memory() -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| -> Option<u64> {
        status.lines().find_map(|line| {
            line.strip_prefix(name)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
    };
    json!({"rss_kib": field("VmRSS:"), "high_water_kib": field("VmHWM:")})
}

/// Only the measured application work and body collection are inside the clock.
async fn request(transport: &Transport) -> (u128, Vec<u8>) {
    match transport {
        Transport::InProcess(app) => {
            let service = app.clone();
            let request = Request::builder().uri(URI).body(Body::empty()).unwrap();
            let started = Instant::now();
            let response = service.oneshot(request).await.unwrap();
            let status = response.status();
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(status, StatusCode::OK);
            (elapsed, body.to_vec())
        }
        Transport::Http { client, url } => {
            let request = client.get(url);
            let started = Instant::now();
            let response = request.send().await.unwrap();
            let status = response.status();
            let body = response.bytes().await.unwrap();
            let elapsed = started.elapsed().as_nanos();
            assert_eq!(status, StatusCode::OK);
            (elapsed, body.to_vec())
        }
    }
}

fn validate(body: &[u8], expected: &[Expected]) {
    let value: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(value["query"], QUERY);
    for key in [
        "unresponsive_engines",
        "answers",
        "corrections",
        "infoboxes",
        "suggestions",
    ] {
        assert_eq!(value[key], json!([]), "unexpected {key}");
    }
    let results = value["results"].as_array().unwrap();
    assert_eq!(results.len(), expected.len());
    for (index, (actual, expected)) in results.iter().zip(expected).enumerate() {
        assert_eq!(actual["url"], expected.url);
        assert_eq!(actual["title"], expected.title);
        assert_eq!(actual["content"], expected.snippet);
        assert_eq!(actual["raw_content"], expected.raw);
        assert_eq!(actual["engine"], "mwmbl");
        assert_eq!(actual["engines"], json!(["mwmbl"]));
        assert_eq!(actual["positions"], json!([index + 1]));
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut iterations = 30usize;
    let mut workload = "standard".to_string();
    let mut transport_name = "in-process".to_string();
    let mut html_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iterations" => iterations = args.next().expect("iteration count").parse().unwrap(),
            "--workload" => workload = args.next().expect("workload name"),
            "--html-dir" => html_dir = Some(args.next().expect("HTML fixture directory").into()),
            "--transport" => transport_name = args.next().expect("transport name"),
            _ => panic!(
                "unknown argument {arg}; use --iterations N --workload standard|mixed|real \
                 [--html-dir PATH] [--transport in-process|http]"
            ),
        }
    }
    assert!(iterations > 0, "iterations must be positive");
    assert!(
        matches!(transport_name.as_str(), "in-process" | "http"),
        "transport must be in-process or http"
    );
    let synthetic_bytes = match workload.as_str() {
        "standard" => vec![32 * 1024; 5],
        "mixed" => vec![4 * 1024, 32 * 1024, 128 * 1024, 512 * 1024, 2 * 1024 * 1024],
        "real" => {
            assert!(
                html_dir.is_some(),
                "--workload real requires --html-dir PATH"
            );
            Vec::new()
        }
        _ => panic!("workload must be standard, mixed, or real"),
    };
    let mut settings = Settings::defaults();
    settings.engines = vec![EngineConfig::new("mwmbl", "mwmbl", "mwm", &["general"])];
    settings.search.raw_content_limit = 5;
    settings.search.raw_content_chars = RAW_CHARS;
    settings.search.embedding_url.clear();
    settings.search.site_scoped_retry = false;
    settings.search.deep_pages = 0;
    settings.search.local_index_path.clear();
    settings.search.autocomplete = "none".into();
    settings.server.limiter = LimiterSettings::Enabled(false);
    settings.server.secret_key = "deterministic-benchmark".into();
    settings.cache.backend = "memory".into();
    settings.cache.ttl = 3600;
    settings.cache.max_bytes = 32 * 1024 * 1024;
    let searcher = Searcher::new(settings).unwrap();
    let query = Query::parse(QUERY, &searcher.registry, &[]);
    let engine_request = searcher
        .registry
        .get("mwmbl")
        .unwrap()
        .build_request(&query, 1)
        .unwrap();
    let mut expected = Vec::new();
    let mut page_bytes = Vec::with_capacity(5);
    let mut engine_items = Vec::new();
    let mut inputs = Sha256::new();
    for (index, filename) in REAL_FILES.iter().enumerate() {
        let url = format!("https://example.com/rust-programming-guide-{index}");
        let title = format!("Rust programming guide {index}");
        let snippet =
            format!("Rust programming reference chapter {index}, with practical examples.");
        let (html, raw) = if workload == "real" {
            let path = html_dir.as_ref().unwrap().join(filename);
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let parsed = scraper::Html::parse_document(&text);
            let raw = reference_text(&parsed, RAW_CHARS);
            assert!(
                !raw.is_empty(),
                "{} has no extractable text",
                path.display()
            );
            (bytes, raw)
        } else {
            let (html, raw) = page(index, synthetic_bytes[index]);
            (html.into_bytes(), raw)
        };
        page_bytes.push(html.len());
        inputs.update(url.as_bytes());
        inputs.update(&html);
        seed(&searcher.cache, &url, html, "text/html").await;
        engine_items.push(json!({
            "url": url, "title": [{"value": title}], "extract": [{"value": snippet}],
        }));
        expected.push(Expected {
            url,
            title,
            snippet,
            raw,
        });
    }
    let engine_body = serde_json::to_vec(&engine_items).unwrap();
    inputs.update(&engine_body);
    seed(
        &searcher.cache,
        &engine_request.url,
        engine_body,
        "application/json",
    )
    .await;
    let state = AppState::new(searcher);
    let searcher = state.searcher.clone();
    let app = router(state);
    let transport = if transport_name == "http" {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Transport::Http {
            client: reqwest::Client::builder().build().unwrap(),
            url: format!("http://{address}{URI}"),
        }
    } else {
        Transport::InProcess(app)
    };
    assert_eq!(searcher.cache.stats().entries, 6);
    let before_warmup = memory();
    let mut output_hash = None;
    for _ in 0..WARMUP {
        let (_, body) = request(&transport).await;
        validate(&body, &expected);
        let hash = digest(&body);
        if let Some(previous) = &output_hash {
            assert_eq!(previous, &hash);
        }
        output_hash = Some(hash);
    }
    let after_warmup = memory();
    let mut samples = Vec::with_capacity(iterations);
    for repeat in 0..iterations {
        let (elapsed_ns, body) = request(&transport).await;
        let observed_memory = memory();
        validate(&body, &expected);
        assert_eq!(output_hash.as_ref().unwrap(), &digest(&body));
        samples
            .push(json!({"repeat": repeat, "elapsed_ns": elapsed_ns, "memory": observed_memory}));
    }
    let cache = searcher.cache.stats();
    let metrics = searcher.metrics.snapshot();
    assert_eq!(cache.miss, 0, "unexpected upstream cache miss");
    assert_eq!(cache.error, 0);
    assert_eq!(cache.hit, (6 * (iterations + WARMUP)) as u64);
    assert_eq!(metrics.engine_errors, 0);
    assert_eq!(metrics.engine_timeouts, 0);
    assert_eq!(metrics.searches, (iterations + WARMUP) as u64);
    let expected_results: Vec<Value> = expected
        .iter()
        .map(|item| {
            json!({
                "url": item.url, "title": item.title,
                "raw_content_chars": item.raw.chars().count(),
                "raw_content_sha256": digest(item.raw.as_bytes()),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&json!({
        "transport": transport_name,
        "scope": if transport_name == "http" {
            "real HTTP over loopback TCP with reused client, cached engine and page bodies; includes HTTP send and complete response receive; excludes TLS and live upstream network"
        } else {
            "real in-process Axum router, cached engine and page bodies; excludes TCP/TLS and live network"
        },
        "workload": workload, "page_bytes": page_bytes, "raw_content_char_limit": RAW_CHARS,
        "html_files": if workload == "real" { Some(REAL_FILES) } else { None },
        "memory_scope": "process RSS/high-water includes setup, warmup, requests and validation; real workload additionally includes original HTML parser/extractor oracle during setup; not isolated request memory",
        "iterations": iterations, "warmup_requests": WARMUP,
        "input_sha256": format!("{:x}", inputs.finalize()),
        "output_sha256": output_hash, "expected_results": expected_results,
        "cache": cache, "engine_errors": metrics.engine_errors,
        "memory_before_warmup": before_warmup, "memory_after_warmup": after_warmup,
        "memory_after_requests": memory(), "samples": samples,
    })).unwrap());
}
