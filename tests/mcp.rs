// SPDX-License-Identifier: AGPL-3.0-or-later
//! MCP server tests: a real rmcp client talks to the server over an in-memory
//! pipe, with a fake engine over loopback HTTP. No external network access.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use axum::Router;
use axum::http::header;
use axum::routing::get;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use rustxng::aggregator::Searcher;
use rustxng::cache::{Cache, CachedResponse};
use rustxng::mcp::McpServer;
use rustxng::settings::{EngineConfig, Settings};

/// Mwmbl-format results; the URLs are public so read_page's guard allows them.
const MWMBL_RESPONSE: &str = r#"[
  {
    "url": "https://example.com/rust",
    "title": [{"value": "The Rust Programming Language"}],
    "extract": [{"value": "A language empowering everyone."}]
  },
  {
    "url": "https://example.com/rust-book",
    "title": [{"value": "The Rust Book"}],
    "extract": [{"value": "Learn Rust."}]
  }
]"#;

/// The page behind the first result, served from the response cache.
const PAGE: &str = "<html><body><nav>menu</nav><article>Ownership is Rust's \
                    most unique feature. 所有権 makes memory safety possible.</article></body></html>";
const PAGE_TEXT: &str =
    "Ownership is Rust's most unique feature. 所有権 makes memory safety possible.";

async fn start_fake_engine() -> String {
    let app = Router::new().route(
        "/api/v1/search/",
        get(|| async { ([(header::CONTENT_TYPE, "application/json")], MWMBL_RESPONSE) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/api/v1")
}

async fn connect() -> RunningService<RoleClient, ()> {
    let mut settings = Settings::defaults();
    let mut engine = EngineConfig::new("mwmbl", "mwmbl", "mwm", &["general"]);
    engine.base_url = Some(start_fake_engine().await);
    engine.timeout = Some(2.0);
    settings.engines = vec![engine];
    settings.search.timeout_limit = 2.0;
    settings.search.site_scoped_retry = false;
    settings.search.embedding_url.clear();
    settings.search.local_index_path.clear();
    let searcher = Searcher::new(settings).expect("searcher");
    searcher
        .cache
        .set(
            &Cache::key("GET", "https://example.com/rust", ""),
            CachedResponse {
                status: 200,
                final_url: "https://example.com/rust".into(),
                headers: BTreeMap::from([("content-type".into(), "text/html".into())]),
                body: Arc::new(PAGE.as_bytes().to_vec()),
            },
        )
        .await;

    let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
    let server = McpServer::new(Arc::new(searcher));
    tokio::spawn(async move {
        let running = server.serve(server_transport).await.expect("server");
        let _ = running.waiting().await;
    });
    ().serve(client_transport).await.expect("client")
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
    let client = connect().await;
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
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn search_returns_ranked_snippets_without_page_text() {
    let client = connect().await;
    let result = call(&client, "search", json!({"query": "rust", "limit": 2})).await;
    let value = structured(&result);
    assert_eq!(value["query"], "rust");
    let results = value["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["rank"], 1);
    assert_eq!(results[0]["url"], "https://example.com/rust");
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
    let client = connect().await;
    call(&client, "search", json!({"query": "rust"})).await;
    let url = "https://example.com/rust";

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
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn read_page_refuses_urls_that_search_did_not_return() {
    let client = connect().await;
    for url in ["https://example.com/rust", "http://127.0.0.1/admin"] {
        let result = call(&client, "read_page", json!({"url": url})).await;
        assert!(error_text(&result).contains("only reads URLs"), "{url}");
    }
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn bad_arguments_are_reported_as_errors() {
    let client = connect().await;
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
        json!({"url": "https://example.com/rust", "max_chars": 0}),
    )
    .await;
    error_text(&result);
    let result = call(
        &client,
        "read_page",
        json!({"url": "https://example.com/rust", "offset": 100000}),
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
