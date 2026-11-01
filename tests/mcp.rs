// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP server tests: a real rmcp client talks to the server over an in-memory
//! pipe, with a fake engine over loopback HTTP. No external network access.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::Router;
use axum::http::header;
use axum::response::Redirect;
use axum::routing::get;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use rustxng::aggregator::Searcher;
use rustxng::mcp::McpServer;
use rustxng::settings::{EngineConfig, Settings};

/// The page behind the first result.
const PAGE: &str = "<html><body><nav>menu</nav><article>Ownership is Rust's \
                    most unique feature. 所有権 makes memory safety possible.</article></body></html>";
const PAGE_TEXT: &str =
    "Ownership is Rust's most unique feature. 所有権 makes memory safety possible.";

/// A connected client, the public-looking origin of the result pages
/// (`example.com` pinned to the loopback fake server) and its page counter.
struct Fixture {
    client: RunningService<RoleClient, ()>,
    origin: String,
    port: u16,
    page_fetches: Arc<AtomicUsize>,
}

/// One loopback server plays the engine and the result pages. The engine
/// returns pages on `example.com:PORT`, pages that redirect to loopback, a
/// literal loopback URL and an oversized page.
async fn connect() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("http://example.com:{port}");
    let page_fetches = Arc::new(AtomicUsize::new(0));
    let results = json!([
        {"url": format!("{origin}/rust"),
         "title": [{"value": "The Rust Programming Language"}],
         "extract": [{"value": "A language empowering everyone."}]},
        {"url": format!("{origin}/rust-book"),
         "title": [{"value": "The Rust Book"}],
         "extract": [{"value": "Learn Rust."}]},
        {"url": format!("{origin}/to-loopback"),
         "title": [{"value": "Redirects to a loopback address"}],
         "extract": [{"value": "Rust redirect."}]},
        {"url": format!("{origin}/to-localhost"),
         "title": [{"value": "Redirects to localhost"}],
         "extract": [{"value": "Rust redirect by name."}]},
        {"url": format!("http://127.0.0.1:{port}/rust"),
         "title": [{"value": "A loopback result"}],
         "extract": [{"value": "Rust on loopback."}]},
        {"url": format!("{origin}/huge"),
         "title": [{"value": "A huge page"}],
         "extract": [{"value": "Rust, at length."}]}
    ])
    .to_string();
    let counter = page_fetches.clone();
    let app =
        Router::new()
            .route(
                "/api/v1/search/",
                get(move || async move { ([(header::CONTENT_TYPE, "application/json")], results) }),
            )
            .route(
                "/rust",
                get(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    async { ([(header::CONTENT_TYPE, "text/html")], PAGE) }
                }),
            )
            .route(
                "/to-loopback",
                get(move || async move {
                    Redirect::temporary(&format!("http://127.0.0.1:{port}/rust"))
                }),
            )
            .route(
                "/to-localhost",
                get(move || async move {
                    Redirect::temporary(&format!("http://localhost:{port}/rust"))
                }),
            )
            .route(
                "/huge",
                get(|| async {
                    // 8 MiB of markup with one readable character per 128 bytes:
                    // about 130,000 characters whole, half that within the 4 MiB cap.
                    let node = format!("<i class=\"{}\">x</i>", "a".repeat(115));
                    let filler = node.repeat(8 * 1024 * 1024 / node.len());
                    let page = format!("<html><body><article>{filler}</article></body></html>");
                    ([(header::CONTENT_TYPE, "text/html")], page)
                }),
            );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut settings = Settings::defaults();
    let mut engine = EngineConfig::new("mwmbl", "mwmbl", "mwm", &["general"]);
    engine.base_url = Some(format!("http://127.0.0.1:{port}/api/v1"));
    engine.timeout = Some(2.0);
    settings.engines = vec![engine];
    settings.search.timeout_limit = 2.0;
    settings.search.site_scoped_retry = false;
    settings.search.embedding_url.clear();
    settings.search.local_index_path.clear();
    let searcher = Searcher::new(settings).expect("searcher");

    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let overrides = [(
        "example.com".to_string(),
        std::net::SocketAddr::from(([127, 0, 0, 1], port)),
    )];
    let server = McpServer::with_page_overrides(Arc::new(searcher), &overrides).unwrap();
    tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("server");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_transport).await.expect("client");
    Fixture {
        client,
        origin,
        port,
        page_fetches,
    }
}

async fn call(client: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> CallToolResult {
    let Value::Object(arguments) = args else {
        panic!("arguments must be an object")
    };
    client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(arguments))
        .await
        .expect("tool call")
}

fn structured(result: &CallToolResult) -> &Value {
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result
        .structured_content
        .as_ref()
        .expect("structured content")
}

fn error_text(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    serde_json::to_string(&result.content).unwrap()
}

#[tokio::test]
async fn tools_are_listed_read_only_with_schemas() {
    let Fixture { client, .. } = connect().await;
    let info = client.peer_info().expect("server info");
    assert_eq!(
        info.server_info.as_ref().expect("server info").name,
        "rustxng"
    );
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort();
    assert_eq!(names, ["read_page", "search"]);
    for tool in &tools {
        let annotations = tool.annotations.as_ref().expect("annotations");
        assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
        assert!(tool.output_schema.is_some(), "{}", tool.name);
        let input = serde_json::to_value(&*tool.input_schema).unwrap();
        assert_eq!(input["additionalProperties"], json!(false), "{}", tool.name);
        // No nullable types: strict tool-calling clients reject them.
        assert!(
            !input.to_string().contains("null"),
            "{}: {input}",
            tool.name
        );
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn search_returns_ranked_snippets_without_page_text() {
    let Fixture { client, origin, .. } = connect().await;
    let result = call(&client, "search", json!({"query": "rust", "limit": 2})).await;
    let value = structured(&result);
    assert_eq!(value["query"], "rust");
    let results = value["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["rank"], 1);
    assert_eq!(results[0]["url"], format!("{origin}/rust"));
    assert_eq!(results[0]["title"], "The Rust Programming Language");
    assert_eq!(results[0]["snippet"], "A language empowering everyone.");
    assert_eq!(results[0]["snippet_truncated"], false);
    assert_eq!(results[0]["engines"], json!(["mwmbl"]));
    assert!(results[0].get("raw_content").is_none());
    assert!(value["note"].as_str().unwrap().contains("untrusted"));

    let one = call(&client, "search", json!({"query": "rust", "limit": 1})).await;
    assert_eq!(structured(&one)["results"].as_array().unwrap().len(), 1);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn read_page_pages_through_a_returned_url_by_character() {
    let Fixture {
        client,
        origin,
        page_fetches,
        ..
    } = connect().await;
    call(&client, "search", json!({"query": "rust", "limit": 10})).await;
    let url = format!("{origin}/rust");

    let first = call(&client, "read_page", json!({"url": url, "max_chars": 20})).await;
    let first = structured(&first);
    let expected: String = PAGE_TEXT.chars().take(20).collect();
    assert_eq!(first["text"], expected);
    assert_eq!(first["next_offset"], 20);

    let mut text = expected;
    let mut offset = 20;
    loop {
        let next = call(
            &client,
            "read_page",
            json!({"url": url, "offset": offset, "max_chars": 20}),
        )
        .await;
        let next = structured(&next).clone();
        text.push_str(next["text"].as_str().unwrap());
        match next["next_offset"].as_u64() {
            Some(value) => offset = value as usize,
            None => break,
        }
    }
    assert_eq!(text, PAGE_TEXT, "slices reassemble the page text exactly");

    let whole = call(&client, "read_page", json!({"url": url})).await;
    assert_eq!(structured(&whole)["text"], PAGE_TEXT);
    assert!(structured(&whole).get("next_offset").is_none());
    assert_eq!(
        page_fetches.load(Ordering::SeqCst),
        1,
        "the page is fetched once and then paged from memory"
    );
    client.cancel().await.unwrap();
}

/// Results are third-party data: a returned URL must not reach private
/// addresses directly, through a redirect or through a name that resolves
/// to one, and a huge page is cut at the byte limit, not read whole.
#[tokio::test]
async fn read_page_only_reaches_public_addresses_and_bounds_downloads() {
    let Fixture {
        client,
        origin,
        port,
        page_fetches,
    } = connect().await;
    let result = call(&client, "search", json!({"query": "rust", "limit": 10})).await;
    assert_eq!(structured(&result)["results"].as_array().unwrap().len(), 6);
    for (url, reason) in [
        (format!("http://127.0.0.1:{port}/rust"), "only public"),
        (format!("{origin}/to-loopback"), "could not be fetched"),
        (format!("{origin}/to-localhost"), "could not be fetched"),
    ] {
        let result = call(&client, "read_page", json!({"url": url})).await;
        assert!(error_text(&result).contains(reason), "{url}");
    }
    assert_eq!(page_fetches.load(Ordering::SeqCst), 0);

    let huge = call(
        &client,
        "read_page",
        json!({"url": format!("{origin}/huge"), "offset": 199990}),
    )
    .await;
    let error = error_text(&huge);
    let available: usize = error
        .split('(')
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{error}"));
    assert!(
        (40_000..90_000).contains(&available),
        "only the first 4 MiB are read: {available} characters"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn read_page_refuses_urls_that_search_did_not_return() {
    let Fixture { client, origin, .. } = connect().await;
    for url in [
        format!("{origin}/rust"),
        "http://127.0.0.1/admin".to_string(),
    ] {
        let result = call(&client, "read_page", json!({"url": url})).await;
        assert!(error_text(&result).contains("only reads URLs"), "{url}");
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn bad_arguments_are_reported_as_errors() {
    let Fixture { client, origin, .. } = connect().await;
    for args in [
        json!({"query": "rust", "limit": 0}),
        json!({"query": "rust", "limit": 11}),
        json!({"query": "   "}),
        json!({"query": "rust", "time_range": "decade"}),
    ] {
        let result = call(&client, "search", args.clone()).await;
        error_text(&result);
    }
    call(&client, "search", json!({"query": "rust"})).await;
    let result = call(
        &client,
        "read_page",
        json!({"url": format!("{origin}/rust"), "max_chars": 0}),
    )
    .await;
    error_text(&result);
    let result = call(
        &client,
        "read_page",
        json!({"url": format!("{origin}/rust"), "offset": 100000}),
    )
    .await;
    assert!(error_text(&result).contains("past the end"));

    // Unknown fields are rejected by the schema, not silently ignored.
    let Value::Object(arguments) = json!({"query": "rust", "unknown": 1}) else {
        unreachable!()
    };
    let outcome = client
        .call_tool(CallToolRequestParams::new("search").with_arguments(arguments))
        .await;
    match outcome {
        Err(_) => {}
        Ok(result) => {
            error_text(&result);
        }
    }
    client.cancel().await.unwrap();
}

/// The real binary speaks MCP on stdout and keeps its logs off it.
#[test]
fn the_binary_serves_mcp_on_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rustxng"))
        .arg("mcp")
        .env("RUST_LOG", "debug")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start rustxng mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |message: Value| {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    };
    let mut receive = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line)
            .unwrap_or_else(|error| panic!("stdout must carry only JSON-RPC: {error}: {line:?}"))
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}}}),
    );
    let initialized = receive();
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "rustxng");
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let listed = receive();
    assert_eq!(listed["id"], 2);
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "exit status {status}");
}
