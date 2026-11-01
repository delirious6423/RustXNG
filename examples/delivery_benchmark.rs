// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a browser perceives: time to first byte, time until the search box is
//! on screen, time until the full page, and bytes on the wire.
//!
//! cargo run --release --no-default-features --example delivery_benchmark --
//!     --iterations 30 [--engines 8] [--slowest-ms 400] [--results 10]
//!     [--accept-encoding "gzip, deflate"]
//!
//! A loopback upstream answers each fake JSON engine after a fixed delay
//! (spread evenly up to --slowest-ms, like a warm search where the slowest
//! responding engine sets the page time). Every iteration uses a fresh query,
//! so no engine response is cached. The client sends a browser's
//! Accept-Encoding and does not decompress, so `wire_bytes` is what crosses
//! the network. Excludes TLS, real engine networks and browser rendering.

use std::time::{Duration, Instant};

use axum::extract::Query;
use axum::routing::get;
use serde_json::{Value, json};

use rustxng::aggregator::Searcher;
use rustxng::settings::{EngineConfig, LimiterSettings, Settings};
use rustxng::web::{AppState, router};

const BROWSER_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

#[derive(serde::Deserialize)]
struct UpstreamParams {
    q: String,
    e: usize,
    delay: u64,
    n: usize,
}

const WORDS: &[&str] = &[
    "rust",
    "ownership",
    "borrow",
    "checker",
    "lifetime",
    "async",
    "await",
    "tokio",
    "runtime",
    "thread",
    "safety",
    "memory",
    "compiler",
    "error",
    "trait",
    "generic",
    "macro",
    "crate",
    "cargo",
    "module",
    "struct",
    "enum",
    "pattern",
    "match",
    "closure",
    "iterator",
    "vector",
    "string",
    "slice",
    "reference",
    "pointer",
    "heap",
    "stack",
    "allocation",
    "performance",
    "benchmark",
    "release",
    "debug",
    "build",
    "test",
    "documentation",
    "example",
    "tutorial",
    "guide",
    "beginner",
    "advanced",
    "practical",
    "reference",
    "manual",
    "chapter",
    "section",
    "network",
    "server",
    "client",
    "request",
    "response",
    "database",
    "query",
    "index",
    "search",
    "engine",
    "result",
    "web",
    "framework",
    "library",
    "package",
    "version",
    "stable",
    "nightly",
    "feature",
    "unsafe",
    "concurrency",
    "parallel",
    "channel",
    "mutex",
    "atomic",
    "future",
    "stream",
    "socket",
    "file",
    "path",
    "parse",
    "serialize",
    "json",
    "config",
    "logging",
    "tracing",
    "profile",
    "optimize",
    "inline",
    "cache",
    "latency",
    "throughput",
    "the",
    "and",
    "with",
    "for",
    "how",
    "why",
    "when",
    "using",
    "into",
    "from",
    "about",
    "your",
    "first",
    "best",
    "common",
    "pitfalls",
    "explained",
    "deep",
    "dive",
    "introduction",
    "overview",
    "comparison",
    "versus",
    "patterns",
    "idioms",
    "tips",
];

/// Deterministic text that compresses like prose, not like a repeated template.
fn words(seed: &mut u64, count: usize) -> String {
    (0..count)
        .map(|_| {
            *seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            WORDS[(*seed >> 33) as usize % WORDS.len()]
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One fake engine page: ten results with snippet-sized content.
async fn upstream(Query(params): Query<UpstreamParams>) -> axum::Json<Value> {
    tokio::time::sleep(Duration::from_millis(params.delay)).await;
    let mut seed = params.q.bytes().fold(params.e as u64 + 1, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u64)
    });
    let results: Vec<Value> = (0..params.n)
        .map(|_| {
            let host = words(&mut seed, 1);
            let path = words(&mut seed, 3).replace(' ', "-");
            let title = words(&mut seed, 7);
            let content = words(&mut seed, 28);
            json!({
                "url": format!("https://{host}{}.example.org/{path}", params.e),
                "title": title,
                "content": content,
            })
        })
        .collect();
    axum::Json(json!({ "results": results }))
}

#[derive(Default)]
struct Sample {
    headers_ms: f64,
    first_byte_ms: f64,
    /// Until the bytes received so far, decoded, contain the closing search form.
    shell_ms: Option<f64>,
    total_ms: f64,
    wire_bytes: usize,
    content_encoding: String,
    cache_control: String,
}

/// Decode what has arrived so far, tolerating an unfinished stream.
fn decoded_prefix(encoding: &str, wire: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let _ = flate2::read::GzDecoder::new(wire).read_to_end(&mut out);
        }
        "br" => {
            let _ = brotli::Decompressor::new(wire, 4096).read_to_end(&mut out);
        }
        "zstd" => {
            if let Ok(mut decoder) = zstd::stream::read::Decoder::new(wire) {
                let _ = decoder.read_to_end(&mut out);
            }
        }
        _ => out.extend_from_slice(wire),
    }
    out
}

struct Client {
    http: reqwest::Client,
    accept_encoding: String,
}

async fn fetch(client: &Client, url: &str, marker: Option<&[u8]>) -> Sample {
    let started = Instant::now();
    let mut response = client
        .http
        .get(url)
        .header("Accept-Encoding", &client.accept_encoding)
        .header("Accept", "text/html,application/json,*/*")
        .send()
        .await
        .expect("request");
    assert!(
        response.status().is_success(),
        "{url}: {}",
        response.status()
    );
    let mut sample = Sample {
        headers_ms: started.elapsed().as_secs_f64() * 1e3,
        content_encoding: header(&response, "content-encoding"),
        cache_control: header(&response, "cache-control"),
        ..Sample::default()
    };
    let mut wire = Vec::new();
    while let Some(chunk) = response.chunk().await.expect("body chunk") {
        let now = started.elapsed().as_secs_f64() * 1e3;
        if wire.is_empty() {
            sample.first_byte_ms = now;
        }
        wire.extend_from_slice(&chunk);
        if let (Some(marker), None) = (marker, sample.shell_ms) {
            let decoded = decoded_prefix(&sample.content_encoding, &wire);
            if decoded.windows(marker.len()).any(|window| window == marker) {
                sample.shell_ms = Some(now);
            }
        }
    }
    sample.total_ms = started.elapsed().as_secs_f64() * 1e3;
    sample.wire_bytes = wire.len();
    let decoded = decoded_prefix(&sample.content_encoding, &wire);
    if url.contains("format=json") {
        let value: Value = serde_json::from_slice(&decoded).expect("valid JSON body");
        assert!(!value["results"].as_array().expect("results").is_empty());
    } else if marker.is_some() {
        assert!(String::from_utf8_lossy(&decoded).contains("</html>"));
    }
    sample
}

fn header(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn summary(samples: &[Sample]) -> Value {
    let pick = |f: fn(&Sample) -> f64| median(samples.iter().map(f).collect());
    let shell: Vec<f64> = samples.iter().filter_map(|s| s.shell_ms).collect();
    json!({
        "headers_ms": pick(|s| s.headers_ms),
        "first_byte_ms": pick(|s| s.first_byte_ms),
        "shell_ms": (!shell.is_empty()).then(|| median(shell)),
        "total_ms": pick(|s| s.total_ms),
        "wire_bytes": pick(|s| s.wire_bytes as f64),
        "content_encoding": samples[0].content_encoding,
        "cache_control": samples[0].cache_control,
    })
}

#[tokio::main]
async fn main() {
    let mut iterations = 30usize;
    let mut engines = 8usize;
    let mut slowest_ms = 400u64;
    let mut results = 10usize;
    let mut accept_encoding = BROWSER_ACCEPT_ENCODING.to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--accept-encoding" => accept_encoding = args.next().expect("encodings"),
            _ => {
                let value: u64 = args.next().expect("value").parse().expect("number");
                match arg.as_str() {
                    "--iterations" => iterations = value as usize,
                    "--engines" => engines = value as usize,
                    "--slowest-ms" => slowest_ms = value,
                    "--results" => results = value as usize,
                    _ => panic!(
                        "use --iterations N --engines N --slowest-ms MS --results N \
                         --accept-encoding LIST"
                    ),
                }
            }
        }
    }

    let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_address = upstream_listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            upstream_listener,
            axum::Router::new().route("/search", get(upstream)),
        )
        .await
        .unwrap();
    });

    let mut settings = Settings::defaults();
    settings.engines = (0..engines)
        .map(|index| {
            let delay = slowest_ms * (index as u64 + 1) / engines as u64;
            let mut config = EngineConfig::new(
                &format!("fake{index}"),
                "json_engine",
                &format!("f{index}"),
                &["general"],
            );
            config.search_url = Some(format!(
                "http://{upstream_address}/search?q={{query}}&e={index}&delay={delay}&n={results}"
            ));
            for (key, value) in [
                ("results_query", "results"),
                ("url_query", "url"),
                ("title_query", "title"),
                ("content_query", "content"),
            ] {
                config.extra.insert(key.into(), value.into());
            }
            config
        })
        .collect();
    settings.search.embedding_url.clear();
    settings.search.site_scoped_retry = false;
    settings.search.deep_pages = 0;
    settings.search.local_index_path.clear();
    settings.search.autocomplete = "none".into();
    settings.server.limiter = LimiterSettings::Enabled(false);
    settings.server.secret_key = "deterministic-benchmark".into();
    settings.cache.backend = "memory".into();
    let searcher = Searcher::new(settings).unwrap();
    assert_eq!(
        searcher.registry.len(),
        engines,
        "{:?}",
        searcher.registry.failed()
    );
    let app = router(AppState::new(searcher));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let client = Client {
        http: reqwest::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .build()
            .unwrap(),
        accept_encoding: accept_encoding.clone(),
    };
    let mut html = Vec::new();
    let mut json_samples = Vec::new();
    let mut css = Vec::new();
    for iteration in 0..iterations + 2 {
        let q = format!("rust+programming+{iteration}");
        let page = fetch(&client, &format!("{base}/search?q={q}"), Some(b"</form>")).await;
        let api = fetch(
            &client,
            &format!("{base}/search?q={q}+api&format=json"),
            None,
        )
        .await;
        let style = fetch(&client, &format!("{base}/static/style.css"), None).await;
        // The first two iterations warm connections and are not reported.
        if iteration >= 2 {
            html.push(page);
            json_samples.push(api);
            css.push(style);
        }
    }
    let report = json!({
        "iterations": iterations,
        "engines": engines,
        "slowest_engine_ms": slowest_ms,
        "results_per_engine": results,
        "accept_encoding": accept_encoding,
        "html_search": summary(&html),
        "json_search": summary(&json_samples),
        "style_css": summary(&css),
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
