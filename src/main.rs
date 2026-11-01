//! Binary entry point: `rustxng [--config FILE] [--bind ADDRESS] [--port PORT]`,
//! or `rustxng mcp [--config FILE]` for the MCP server on stdio.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use rustxng::aggregator::Searcher;
use rustxng::settings::Settings;
use rustxng::version::version_string;
use rustxng::web::{self, AppState};

const HELP: &str = "rustxng - a Rust rewrite of the SearXNG metasearch engine

USAGE:
    rustxng [OPTIONS]
    rustxng mcp [--config FILE]    Serve the MCP tools on stdin/stdout

OPTIONS:
    -c, --config <FILE>    Settings file (default: $SEARXNG_SETTINGS_PATH)
    -b, --bind <ADDRESS>   Address to listen on (default: server.bind_address)
    -p, --port <PORT>      Port to listen on (default: server.port)
    -v, --version          Print the version
    -h, --help             Print this help

ENVIRONMENT:
    SEARXNG_SETTINGS_PATH, SEARXNG_BIND_ADDRESS, SEARXNG_PORT, SEARXNG_SECRET,
    SEARXNG_BASE_URL, SEARXNG_DEBUG, SEARXNG_ENABLE_METRICS, RUST_LOG
";

struct Args {
    /// `rustxng mcp`: serve MCP on stdio instead of HTTP.
    mcp: bool,
    config: Option<PathBuf>,
    bind: Option<String>,
    port: Option<u16>,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut args = Args {
        mcp: false,
        config: None,
        bind: None,
        port: None,
    };
    let mut argv = std::env::args().skip(1).peekable();
    if argv.peek().map(String::as_str) == Some("mcp") {
        argv.next();
        args.mcp = true;
    }
    while let Some(argument) = argv.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(None);
            }
            "-v" | "--version" => {
                println!("{}", version_string());
                return Ok(None);
            }
            "-c" | "--config" => {
                args.config = Some(PathBuf::from(
                    argv.next().ok_or("--config needs a file path")?,
                ));
            }
            "-b" | "--bind" => args.bind = Some(argv.next().ok_or("--bind needs an address")?),
            "-p" | "--port" => {
                let value = argv.next().ok_or("--port needs a number")?;
                args.port = Some(value.parse().map_err(|_| "--port needs a number")?);
            }
            other if other.starts_with("--config=") => {
                args.config = Some(PathBuf::from(&other["--config=".len()..]));
            }
            other if other.starts_with("--port=") => {
                args.port = Some(
                    other["--port=".len()..]
                        .parse()
                        .map_err(|_| "--port needs a number")?,
                );
            }
            other => return Err(format!("unknown argument {other:?} (try --help)")),
        }
    }
    Ok(Some(args))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,rustxng=debug"));
    if args.mcp {
        // stdout carries the MCP messages; logs must not mix into them.
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let settings_path = args.config.or_else(Settings::path_from_env);
    let settings = match Settings::load(settings_path.as_deref()) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let bind_address = args
        .bind
        .unwrap_or_else(|| settings.server.bind_address.clone());
    let port = args.port.unwrap_or(settings.server.port);
    let address: SocketAddr = match format!("{bind_address}:{port}").parse() {
        Ok(address) => address,
        Err(error) => {
            eprintln!("error: invalid listen address {bind_address}:{port}: {error}");
            return ExitCode::FAILURE;
        }
    };

    let searcher = match Searcher::new(settings) {
        Ok(searcher) => searcher,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    if args.mcp {
        tracing::info!(engines = searcher.registry.len(), "MCP server on stdio");
        return match rustxng::mcp::serve_stdio(searcher).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
    tracing::info!(engines = searcher.registry.len(), "{}", version_string());

    let state = AppState::new(searcher);
    // Track the last request so an idle sidecar can exit on its own.
    let idle_seconds = idle_timeout_seconds(&state);
    let activity = std::sync::Arc::new(AtomicU64::new(unix_now()));
    let touched = activity.clone();
    let app = web::router(state)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let touched = touched.clone();
                async move {
                    touched.store(unix_now(), Ordering::Relaxed);
                    next.run(request).await
                }
            },
        ));

    let listener = match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("error: could not listen on {address}: {error}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!("listening on http://{address}");

    if let Err(error) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_reason(activity, idle_seconds))
    .await
    {
        eprintln!("error: server failed: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Seconds since the epoch.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The idle timeout the settings or RUSTXNG_IDLE_TIMEOUT ask for.
fn idle_timeout_seconds(state: &AppState) -> u64 {
    if let Ok(value) = std::env::var("RUSTXNG_IDLE_TIMEOUT") {
        if let Ok(seconds) = value.trim().parse() {
            return seconds;
        }
    }
    state.searcher.settings.server.idle_timeout
}

/// Resolve when the process should stop: a signal, or an idle timeout.
async fn shutdown_reason(activity: std::sync::Arc<AtomicU64>, idle_seconds: u64) {
    if idle_seconds == 0 {
        shutdown_signal().await;
        return;
    }
    tokio::select! {
        () = shutdown_signal() => {}
        () = async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let idle = unix_now().saturating_sub(activity.load(Ordering::Relaxed));
                if idle >= idle_seconds {
                    tracing::info!(idle, "idle timeout reached, shutting down");
                    return;
                }
            }
        } => {}
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
