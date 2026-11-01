//! `sentry-ducklake` binary: a thin wrapper around the `sdl_server` library
//! that adds logging, Prometheus metrics, a listener and signal handling.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mallard_common::{LogFormat, init_tracing, install_prometheus, shutdown_signal};
use sdl_server::{Server, Standalone};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "sentry-ducklake", version, about = "Sentry-compatible error tracking on DuckLake")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Flags for the default `serve` command (also accepted after `serve`).
    #[command(flatten)]
    serve: Option<ServeArgs>,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[command(flatten)]
    config: Standalone,
    /// Serve `/metrics` on this separate address instead of the main port
    /// (keeps it off the public ingress), e.g. `0.0.0.0:9090`.
    #[arg(long, env = "SDL_METRICS_LISTEN")]
    metrics_listen: Option<SocketAddr>,
    /// Log as JSON lines.
    #[arg(long, env = "SDL_LOG_JSON")]
    log_json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (default when no command is given).
    Serve(Box<ServeArgs>),
    /// Download the DuckDB extensions the server can use (used when
    /// building container images).
    InstallExtensions {
        /// Where to put them (default: DuckDB's `~/.duckdb/extensions`).
        #[arg(long, env = "SDL_EXTENSION_DIR")]
        extension_dir: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let args = match cli.command {
        Some(Command::InstallExtensions { extension_dir }) => {
            init_tracing(LogFormat::Text);
            let duck = mallard_duck::DuckDbConfig {
                extension_directory: extension_dir.map(|d| d.display().to_string()).unwrap_or_default(),
                ..Default::default()
            };
            return tokio::task::spawn_blocking(move || {
                mallard_duck::install_extensions(&duck, mallard_duck::ALL_EXTENSIONS)
            })
            .await?;
        }
        Some(Command::Serve(a)) => *a,
        None => cli.serve.context("missing arguments")?,
    };
    init_tracing(if args.log_json { LogFormat::Json } else { LogFormat::Text });
    let cfg = args.config;
    anyhow::ensure!(!cfg.host().api_tokens.is_empty(), "SDL_API_TOKEN (--api-token) is required");
    let prometheus = install_prometheus()?;
    let listen = cfg.listen;
    let server = Server::new(&cfg).await?;
    let metrics_route =
        axum::Router::new().route("/metrics", axum::routing::get(move || std::future::ready(prometheus.render())));
    let mut app = server.router();
    match args.metrics_listen {
        Some(addr) => {
            let l = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
            tracing::info!(%addr, "metrics listening");
            tokio::spawn(async move {
                if let Err(e) = axum::serve(l, metrics_route).await {
                    tracing::error!(error = %e, "metrics server failed");
                }
            });
        }
        None => app = app.merge(metrics_route),
    }
    let app =
        app.layer(tower_http::trace::TraceLayer::new_for_http()).layer(tower_http::catch_panic::CatchPanicLayer::new());
    let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("binding {listen}"))?;
    tracing::info!(%listen, "listening");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    server.shutdown().await;
    tracing::info!("stopped");
    Ok(())
}
