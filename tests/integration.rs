//! End-to-end tests: a fake engine served over HTTP, the whole search pipeline
//! and the public HTTP API.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use http_body_util::BodyExt;
use tower::ServiceExt;

use rustxng::aggregator::Searcher;
use rustxng::settings::{EngineConfig, LimiterSettings, Settings};
use rustxng::web::{AppState, router};

/// The JSON payload the fake Mwmbl engine answers with.
const MWMBL_RESPONSE: &str = r#"[
  {
    "url": "https://example.com/rust",
    "title": [{"value": "The Rust "}, {"value": "Programming Language"}],
    "extract": [{"value": "A language empowering everyone."}]
  },
  {
    "url": "https://example.com/rust-book",
    "title": [{"value": "The Rust Book"}],
    "extract": [{"value": "Learn Rust."}]
  }
]"#;

/// Start the fake engine and return its base URL.
async fn start_fake_engine() -> String {
    let app = Router::new().route(
        "/api/v1/search/",
        get(|| async { ([(header::CONTENT_TYPE, "application/json")], MWMBL_RESPONSE) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}/api/v1")
}

/// Settings with a single engine pointing at the fake server.
fn settings_for(base_url: &str) -> Settings {
    let mut settings = Settings::defaults();
    let mut engine = EngineConfig::new("mwmbl", "mwmbl", "mwm", &["general"]);
    engine.base_url = Some(base_url.to_string());
    engine.timeout = Some(1.0);
    settings.engines = vec![engine];
    settings.search.autocomplete = "none".to_string();
    settings.search.timeout_limit = 1.0;
    settings.server.secret_key = "test".to_string();
    settings
}

fn app(settings: Settings) -> Router {
    let searcher = Searcher::new(settings).expect("searcher");
    router(AppState::new(searcher))
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

async fn get_uri(app: &Router, uri: &str) -> (StatusCode, String, axum::http::HeaderMap) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = body_text(response).await;
    (status, body, headers)
}

#[tokio::test]
async fn json_search_returns_merged_results() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, headers) = get_uri(&app, "/search?q=rust&format=json&full=1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["search_query"], "rust");
    assert_eq!(json["results"].as_array().unwrap().len(), 2);
    assert_eq!(json["results"][0]["url"], "https://example.com/rust");
    assert_eq!(json["results"][0]["title"], "The Rust Programming Language");
    assert_eq!(json["results"][0]["engines"][0], "mwmbl");
    assert!(json["number_of_results"].as_u64().unwrap() >= 2);
}

#[tokio::test]
async fn agent_facing_filters_limit_and_scope_the_results() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    // max_results caps the JSON payload.
    let (_, body, _) = get_uri(&app, "/search?q=rust&format=json&max_results=1").await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 1);

    // include_domains keeps only matching hosts (subdomains included).
    let (_, body, _) = get_uri(
        &app,
        "/search?q=rust&format=json&include_domains=example.com",
    )
    .await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 2);

    let (_, body, _) = get_uri(
        &app,
        "/search?q=rust&format=json&include_domains=other.test",
    )
    .await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 0);

    // exclude_domains drops the known host again.
    let (_, body, _) = get_uri(
        &app,
        "/search?q=rust&format=json&exclude_domains=example.com",
    )
    .await;
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn research_returns_evidence_without_an_llm() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, headers) = get_uri(&app, "/research?q=rust").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );

    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    // No LLM configured: sub-queries still run and an evidence bundle returns.
    assert!(json["llm_used"] == false);
    assert!(json["report"].is_null());
    assert!(!json["queries"].as_array().unwrap().is_empty());
    let sources = json["sources"].as_array().unwrap();
    assert!(!sources.is_empty());
    assert!(sources[0]["url"].is_string());
    assert!(sources[0]["content"].is_string());

    // A missing question is a client error.
    let (status, _, _) = get_uri(&app, "/research").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn staged_strategy_returns_the_same_results() {
    let base_url = start_fake_engine().await;
    let mut settings = settings_for(&base_url);
    settings.search.strategy = rustxng::settings::SearchStrategy::Staged;
    settings.search.staged_tier = 1;
    settings.search.staged_min_results = 1;
    let app = app(settings);

    let (status, body, _) = get_uri(&app, "/search?q=rust&format=json").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn html_search_renders_the_result_page() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, headers) = get_uri(&app, "/search?q=rust").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(body.contains("The Rust Programming Language"));
    assert!(body.contains("example.com/rust"));
    assert!(body.contains("name=\"q\""));
    assert!(body.contains("mwmbl"));
}

#[tokio::test]
async fn csv_and_rss_formats_are_served() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, headers) = get_uri(&app, "/search?q=rust&format=csv").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/csv")
    );
    assert!(body.starts_with("title,url,content,host,engine,score,type"));
    assert!(body.contains("The Rust Programming Language"));

    let (status, body, headers) = get_uri(&app, "/search?q=rust&format=rss").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/xml")
    );
    assert!(body.contains("<rss version=\"2.0\""));
    assert!(body.contains("<link>https://example.com/rust</link>"));
}

#[tokio::test]
async fn static_pages_and_metadata_are_served() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, _) = get_uri(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "OK");

    let (status, body, _) = get_uri(&app, "/config").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["engines"][0]["name"], "mwmbl");
    assert_eq!(json["engines"][0]["shortcut"], "mwm");

    let (status, body, _) = get_uri(&app, "/opensearch.xml").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<OpenSearchDescription"));
    assert!(body.contains("/search?q={searchTerms}"));

    let (status, _, _) = get_uri(&app, "/static/style.css").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _) = get_uri(&app, "/about").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("AGPL-3.0-or-later"));

    let (status, body, _) = get_uri(&app, "/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Statistics"));
}

#[tokio::test]
async fn missing_query_and_unknown_routes_are_reported() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, _, _) = get_uri(&app, "/search?format=json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body, _) = get_uri(&app, "/definitely/not/a/route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("page not found"));

    let (status, _, _) = get_uri(&app, "/search?q=rust&format=xml").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn preferences_are_stored_in_cookies() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let body = "action=save&language=ja&safesearch=2&category=general&enabled_engine=mwmbl&enabled_plugin=hostnames";
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/preferences")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookies: Vec<String> = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies
            .iter()
            .any(|cookie| cookie.starts_with("language=ja"))
    );
    assert!(
        cookies
            .iter()
            .any(|cookie| cookie.starts_with("safesearch=2"))
    );
}

#[tokio::test]
async fn an_unreachable_engine_is_reported_as_unresponsive() {
    // Port 9 is the discard service: nothing is listening there.
    let app = app(settings_for("http://127.0.0.1:9/api/v1"));
    let (status, body, _) = get_uri(&app, "/search?q=rust&format=json&full=1").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(json["results"].as_array().unwrap().is_empty());
    // Upstream shape: [[engine, "Translated error"], ...]
    let unresponsive = json["unresponsive_engines"].as_array().unwrap();
    assert_eq!(unresponsive[0][0], "mwmbl");
    assert!(
        unresponsive[0][1].as_str().unwrap().contains("rror")
            || unresponsive[0][1].as_str().unwrap().contains("imeout")
    );
}

#[tokio::test]
async fn bangs_select_engines_and_languages() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));

    let (status, body, _) = get_uri(&app, "/search?q=%21mwmbl+rust&format=json&full=1").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["results"].as_array().unwrap().len(), 2);

    // An unknown bang is not a bang: it stays part of the search terms.
    let (status, body, _) = get_uri(&app, "/search?q=%21nosuch+rust&format=json&full=1").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["search_query"], "!nosuch rust");
}

#[test]
fn the_default_settings_build_every_engine() {
    let searcher = Searcher::new(Settings::defaults()).expect("searcher");
    assert!(searcher.registry.len() >= 20);
    assert!(Arc::strong_count(&searcher.registry) >= 1);
}

#[tokio::test]
async fn metrics_endpoint_exposes_engine_counters() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));
    get_uri(&app, "/search?q=rust&format=json&full=1").await;

    let (status, body, headers) = get_uri(&app, "/metrics").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/openmetrics-text")
    );
    assert!(body.contains("# HELP searxng_engines_request_count_total"));
    // The exact engine count depends on how many passes ran (the site-scoped
    // retry may query the same engine twice), so assert the shape, not a value.
    let result_line = body
        .lines()
        .find(|line| line.starts_with("searxng_engines_result_count_total{engine=\"mwmbl\"}"))
        .expect("the mwmbl result counter is exported");
    let count: u64 = result_line
        .rsplit(' ')
        .next()
        .and_then(|value| value.parse().ok())
        .expect("the counter value is a number");
    assert!(count >= 2, "expected at least two results, got {count}");
    let searches: u64 = body
        .lines()
        .find(|line| line.starts_with("searxng_search_requests_total"))
        .and_then(|line| line.rsplit(' ').next())
        .and_then(|value| value.parse().ok())
        .expect("the search counter is exported");
    assert!(
        searches >= 1,
        "expected at least one search, got {searches}"
    );
    assert!(body.trim_end().ends_with("# EOF"));
}

#[tokio::test]
async fn image_proxy_refuses_unsigned_and_private_urls() {
    let base_url = start_fake_engine().await;
    let mut settings = settings_for(&base_url);
    settings.server.image_proxy = true;
    settings.server.secret_key = "test-secret".to_string();
    let app = app(settings);

    let (status, _, _) = get_uri(&app, "/image_proxy?url=https%3A%2F%2Fexample.com%2Fa.png").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "missing signature must be refused"
    );

    let (status, _, _) = get_uri(
        &app,
        "/image_proxy?url=https%3A%2F%2Fexample.com%2Fa.png&h=deadbeef",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "wrong signature must be refused"
    );

    // A correctly signed private address is still refused (SSRF guard), which
    // is stricter than upstream.
    let url = "http://127.0.0.1:9/secret";
    let signature = rustxng::web::image_proxy::signature("test-secret", url);
    let uri = format!(
        "/image_proxy?url={}&h={signature}",
        url.replace(':', "%3A").replace('/', "%2F")
    );
    let (status, _, _) = get_uri(&app, &uri).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "private addresses must be refused"
    );
}

#[tokio::test]
async fn image_proxy_is_disabled_by_default() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));
    let (status, _, _) = get_uri(
        &app,
        "/image_proxy?url=https%3A%2F%2Fexample.com%2Fa.png&h=x",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_rate_limiter_answers_429_after_the_limit() {
    let base_url = start_fake_engine().await;
    let mut settings = settings_for(&base_url);
    settings.server.limiter = LimiterSettings::Enabled(true);
    settings.server.limiter_max = 2;
    settings.server.limiter_window = 60;
    let app = app(settings);

    let mut statuses = Vec::new();
    for _ in 0..3 {
        let (status, _, _) = get_uri(&app, "/search?q=rust&format=json&full=1").await;
        statuses.push(status);
    }
    assert_eq!(
        statuses,
        vec![
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS
        ]
    );

    // /healthz is not limited
    let (status, _, _) = get_uri(&app, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_search_form_offers_a_time_range() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));
    let (status, body, _) = get_uri(&app, "/search?q=rust&time_range=week").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("name=\"time_range\""));
    assert!(body.contains("value=\"week\" selected"));
    assert!(body.contains("Past week"));
}

/// Decode what has arrived so far of a possibly unfinished compressed body.
fn decoded_prefix(encoding: &str, wire: &[u8]) -> String {
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
    String::from_utf8_lossy(&out).to_string()
}

/// The page head and search form are sent while the engines are still
/// running, so the browser paints the page instead of waiting for them. This
/// holds for every encoding a browser may negotiate: a compressor that
/// buffered the head until the end would defeat the streaming.
#[tokio::test]
async fn html_search_sends_the_search_form_before_the_engines_answer() {
    for encoding in ["identity", "gzip", "br", "zstd"] {
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let engine = Router::new().route(
            "/api/v1/search/",
            get(move || {
                let gate = gate.clone();
                async move {
                    gate.notified().await;
                    ([(header::CONTENT_TYPE, "application/json")], MWMBL_RESPONSE)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, engine).await.unwrap() });
        let mut settings = settings_for(&format!("http://{address}/api/v1"));
        settings.search.timeout_limit = 5.0;
        settings.engines[0].timeout = Some(5.0);
        let app = app(settings);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/search?q=rust")
                    .header(header::ACCEPT_ENCODING, encoding)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_encoding = response
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|value| value.to_str().unwrap().to_string())
            .unwrap_or_else(|| "identity".into());
        assert_eq!(content_encoding, encoding);
        let mut body = response.into_body();
        let mut wire = Vec::new();
        // Read frames until the form is decodable, while the engine is held back.
        let head = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let frame = body.frame().await.unwrap().unwrap();
                if let Ok(data) = frame.into_data() {
                    wire.extend_from_slice(&data);
                }
                let decoded = decoded_prefix(encoding, &wire);
                if decoded.contains("</form>") {
                    return decoded;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{encoding}: the form must arrive before the engine answers"));
        assert!(
            head.contains("name=\"q\" value=\"rust\""),
            "{encoding}: {head}"
        );
        assert!(!head.contains("The Rust Book"), "{encoding}");
        assert!(
            head.contains("id=\"pending\""),
            "{encoding}: shows that it is searching"
        );

        release.notify_one();
        let rest = body.collect().await.unwrap().to_bytes();
        wire.extend_from_slice(&rest);
        let page = decoded_prefix(encoding, &wire);
        assert!(page.contains("The Rust Book"), "{encoding}: {page}");
        assert!(page.trim_end().ends_with("</html>"), "{encoding}");
        assert!(page.contains("#pending{display:none}"), "{encoding}");
    }
}

/// Text responses are compressed for clients that accept it, and the style
/// sheet can be cached by the browser.
#[tokio::test]
async fn text_responses_are_compressed_and_the_style_sheet_is_cacheable() {
    use std::io::Read;
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));
    for uri in [
        "/static/style.css",
        "/search?q=rust&format=json",
        "/search?q=rust",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(
            response.headers()[header::CONTENT_ENCODING],
            "gzip",
            "{uri}"
        );
        if uri == "/static/style.css" {
            assert!(
                response.headers()[header::CACHE_CONTROL]
                    .to_str()
                    .unwrap()
                    .contains("max-age=")
            );
        }
        let wire = response.into_body().collect().await.unwrap().to_bytes();
        let mut text = String::new();
        flate2::read::GzDecoder::new(&wire[..])
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.contains("rust") || text.contains("body"), "{uri}");
    }
}

/// Pages link the style sheet by content hash, so it can be cached long
/// without serving an old style sheet after an upgrade.
#[tokio::test]
async fn pages_link_the_style_sheet_by_content_hash() {
    let base_url = start_fake_engine().await;
    let app = app(settings_for(&base_url));
    let href = rustxng::web::style_href();
    assert!(href.starts_with("/static/style.css?v="));
    for uri in [
        "/",
        "/search?q=rust",
        "/about",
        "/preferences",
        "/stats",
        "/nope",
    ] {
        let (_, body, _) = get_uri(&app, uri).await;
        assert!(body.contains(&format!("href=\"{href}\"")), "{uri}");
    }
    let (status, _, headers) = get_uri(&app, &href).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
}
