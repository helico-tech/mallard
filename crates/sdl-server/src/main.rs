//! `sentry-ducklake` binary: a thin wrapper around the `sdl_server` library
//! that adds logging, Prometheus metrics, a listener and signal handling.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sdl_server::{Config, Server};
use std::net::SocketAddr;

#[derive(Parser)]
#[command(name = "sentry-ducklake", version, about = "Sentry-compatible error tracking on DuckLake")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Flags for the default `serve` command (also accepted after `serve`).
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(clap::Args)]
struct ServeArgs {
    #[command(flatten)]
    config: Config,
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
    /// Download the DuckDB extensions the server can use into
    /// `--extension-dir` (used when building the Docker image).
    InstallExtensions(Box<ServeArgs>),
}

fn init_tracing(json: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sdl_server=info"));
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    if json { b.json().init() } else { b.init() }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutdown signal received");
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let (args, install) = match cli.command {
        None => (cli.serve, false),
        Some(Command::Serve(a)) => (*a, false),
        Some(Command::InstallExtensions(a)) => (*a, true),
    };
    init_tracing(args.log_json);
    let cfg = args.config;
    match install {
        true => sdl_server::install_all_extensions(&cfg),
        false => {
            anyhow::ensure!(!cfg.api_tokens().is_empty(), "SDL_API_TOKEN (--api-token) is required");
            let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
                .set_buckets(&[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])?
                .set_buckets_for_metric(
                    metrics_exporter_prometheus::Matcher::Full("sdl_write_batch_events".into()),
                    &[1.0, 5.0, 10.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2000.0, 5000.0],
                )?
                .install_recorder()
                .context("installing metrics recorder")?;
            let listen = cfg.listen;
            let server = Server::new(cfg).await?;
            let metrics_route = axum::Router::new()
                .route("/metrics", axum::routing::get(move || std::future::ready(prometheus.render())));
            let mut app = server.router();
            match args.metrics_listen {
                Some(addr) => {
                    let l = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))?;
                    tracing::info!(%addr, "metrics listening");
                    tokio::spawn(async move { axum::serve(l, metrics_route).await });
                }
                None => app = app.merge(metrics_route),
            }
            let app = app
                .layer(tower_http::trace::TraceLayer::new_for_http())
                .layer(tower_http::catch_panic::CatchPanicLayer::new());
            let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("binding {listen}"))?;
            tracing::info!(%listen, "listening");
            axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                .with_graceful_shutdown(shutdown_signal())
                .await?;
            server.shutdown().await;
            tracing::info!("stopped");
            Ok(())
        }
    }
}
