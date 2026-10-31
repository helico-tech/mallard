//! `sentry-ducklake` binary: a thin wrapper around the `sdl_server` library
//! that adds logging, Prometheus metrics, a listener and signal handling.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sdl_server::{Config, Server};
use std::net::SocketAddr;

#[derive(Parser)]
#[command(name = "sentry-ducklake", version, about = "Sentry-compatible error tracking on DuckLake")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    config: Config,
    /// Log as JSON lines.
    #[arg(long, env = "SDL_LOG_JSON", global = true)]
    log_json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (default).
    Serve,
    /// Download all DuckDB extensions any configuration may need into
    /// `--extension-dir` (used when building the Docker image).
    InstallExtensions,
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
    init_tracing(cli.log_json);
    let cfg = cli.config;
    match cli.command.unwrap_or(Command::Serve) {
        Command::InstallExtensions => sdl_server::db::install_all_extensions(&cfg),
        Command::Serve => {
            anyhow::ensure!(!cfg.api_tokens().is_empty(), "SDL_API_TOKEN (--api-token) is required");
            let prometheus = metrics_exporter_prometheus::PrometheusBuilder::new()
                .set_buckets(&[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0])?
                .install_recorder()
                .context("installing metrics recorder")?;
            let listen = cfg.listen;
            let server = Server::new(cfg).await?;
            let app = server
                .router()
                .route("/metrics", axum::routing::get(move || std::future::ready(prometheus.render())))
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
